# strih-tally Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The ESP8266 camera tally lights show the strih OBS program (red) / preview (green) camera, served by a `strih-tally` service on strih-snv and strih-pp, with unchanged tally firmware.

**Architecture:** One asyncio Python process per strih: a WebSocket relay on :9240 (the WsTally protocol, plus `GET /state`), an OBS-WebSocket v5 client to the local strih OBS, and a pure decision module that maps a scene to a camera number. A second unit announces `atemrpi.local` via `avahi-publish`. setup-strih installs both (a new step `16f`, strih role only), verify-strih grades them (a new item 42), through one lib `scripts/lib/strih-tally.sh`.

**Tech Stack:** Python 3.14 (Ubuntu 26.04), `python3-websockets` 15 (`websockets.asyncio.server` / `.client`), systemd system units, avahi-utils, bash libs, pytest.

**Spec:** `docs/superpowers/specs/2026-10-12-strih-tally-design.md` (issue 1417). Read it first.

## Global Constraints

- Port `9240`, path `/`, plain-text frames — the firmware hard-codes them; never change.
- mDNS name `atemrpi.local` — the firmware hard-codes it; never change.
- Messages exactly `TALLY-PGM <n>`, `TALLY-PVW <n>`, `TALLY-TRS`; `n` = camera number, `0` = none.
- Order on a change: `TALLY-PVW` before `TALLY-PGM` (the firmware stores the old pgm as `trs` on PGM).
- Camera identity = the NDI source name `CAM<n> (...)` of an `ndi_source` input, never the OBS input name.
- A scene lights camera n only when its enabled content (recursive: nested scenes + groups) holds exactly ONE distinct camera n; 0 otherwise.
- Studio mode off → preview 0.
- Relay: every text a client sends goes to every OTHER client, never back to the sender.
- Resend the current state every 10 s; OBS unreachable > 10 s → send `TALLY-PVW 0` + `TALLY-PGM 0` once.
- OBS at `ws://127.0.0.1:4455`, no auth (the strih OBS runs `auth_required: false`).
- Install only on the `strih` role (never stream-pp); system units run as the operator user; `Restart=always`.
- `scripts/setup-strih.sh` and `scripts/verify-strih.sh` stay ≤ 1000 lines (`tests/python/test_strih_session_apps_1399.py:716-717`).
- The firmware's WiFi credentials never enter git.
- Tier-0: no local cargo; tests are pytest only. A test that opens sockets binds `127.0.0.1` on port 0.
- Every state change, tally JOIN/leave (with IP), OBS connect/disconnect and the gray fallback is one log line.

## Review Focus

1. A tally that connects BEFORE the first OBS snapshot exists: it must get `TALLY-PVW 0`/`TALLY-PGM 0`, never nothing and never a crash. → Task 3 test `test_tally_joining_before_obs_gets_zeros`.
2. The program scene is a nested scene whose inner scene holds the only camera (e.g. `Development`-style nesting): camera found through the nesting. → Task 2 test `test_nested_scene_camera_found`.
3. A camera input present twice in one scene (two items of the same input, or two inputs on the same `CAM3 (usb)`): still ONE distinct camera → 3. → Task 2 test `test_same_camera_twice_is_one_camera`.
4. A client sends a non-tally text (Companion `SET-LUMINANCE 120`): relayed to the others, the server's own state untouched, no echo to the sender. → Task 3 test `test_relay_forwards_to_others_only`.
5. OBS restarts (connection drops, comes back with a different program scene): the service reconnects and sends the NEW state, no stale red. → Task 3 test `test_obs_reconnect_sends_new_state`.

---

## File Structure

| File | Responsibility |
|---|---|
| `scripts/strih_tally_decision.py` (create) | Pure: scene tree → camera; state diff → messages. No I/O. |
| `scripts/strih_tally.py` (create) | The service: relay server + `/state`, OBS client, resend + fallback loops, CLI. |
| `systemd/strih-tally.service.in` (create) | Unit template (`@USER@`). |
| `systemd/strih-tally-mdns.service` (create) | `avahi-publish -a -R atemrpi.local ${STRIH_TALLY_IP}`. |
| `scripts/lib/strih-tally.sh` (create) | Source-only lib: install (step 16f) + grade rows (item 42). |
| `scripts/lib/obs-box-role.sh` (modify) | `16f` = strih role only. |
| `scripts/lib/strih-provision.sh` (modify) | Source the new lib next to `strih-program-audio.sh`. |
| `scripts/setup-strih.sh` / `scripts/verify-strih.sh` (modify) | One step / one item each, net ≤ 1000 lines. |
| `tally/esp/WsTally/WsTally.ino` (create) | The firmware, credentials replaced by `#include "secrets.h"`. |
| `tally/esp/WsTally/secrets.h.example` (create), `.gitignore` (modify) | Credential template; the real `secrets.h` ignored. |
| `tests/python/test_strih_tally_decision_1417.py`, `test_strih_tally_service_1417.py`, `test_strih_tally_install_1417.py`, `test_strih_tally_firmware_1417.py` (create) | Tests per task. |
| `.claude/rules/strih-tally.md` (create), `CLAUDE.md` (modify) | Playbook rule + router line. |

---

### Task 1: Firmware into the repo, credentials out

**Files:**
- Create: `tally/esp/WsTally/WsTally.ino`, `tally/esp/WsTally/secrets.h.example`, `tally/esp/README.md`
- Modify: `.gitignore`
- Test: `tests/python/test_strih_tally_firmware_1417.py`

**Interfaces:** Produces nothing other tasks import.

- [ ] **Step 1: Write the failing test**

```python
"""issue 1417: the tally firmware lives in the repo without the venue WiFi credentials."""
import re
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
INO = REPO / "tally/esp/WsTally/WsTally.ino"


def test_firmware_is_in_the_repo():
    s = INO.read_text()
    assert '#define HOST "atemrpi.local"' in s
    assert "#define PORT 9240" in s
    assert 'String message = "JOIN " + hostname;' in s


def test_firmware_reads_credentials_from_secrets_h():
    s = INO.read_text()
    assert '#include "secrets.h"' in s
    assert re.search(r'^\s*#define\s+(SSID|PASS)\s+"', s, re.M) is None
    assert "WiFi.begin(WIFI_SSID, WIFI_PASS);" in s


def test_secrets_h_is_ignored_and_the_example_is_not():
    ign = subprocess.run(["git", "check-ignore", "tally/esp/WsTally/secrets.h"], cwd=REPO)
    assert ign.returncode == 0
    ex = subprocess.run(["git", "check-ignore", "tally/esp/WsTally/secrets.h.example"], cwd=REPO)
    assert ex.returncode == 1
    t = (REPO / "tally/esp/WsTally/secrets.h.example").read_text()
    assert '#define WIFI_SSID "' in t and '#define WIFI_PASS "' in t
```

- [ ] **Step 2: Run it** — `python3 -m pytest -q tests/python/test_strih_tally_firmware_1417.py` → FAIL (file missing).

- [ ] **Step 3: Add the firmware**

Unzip `/home/newlevel/uploads/wstally/WsTally-main.zip` into a scratch dir (read only; never run anything from it). Copy `WsTally-main/WsTally.ino` to `tally/esp/WsTally/WsTally.ino` and change exactly:
- the two lines `#define SSID "..."` and `#define PASS "..."` → one line `#include "secrets.h"` (never paste the values anywhere: not in a commit, a comment, a test or a log);
- `WiFi.begin(SSID, PASS);` → `WiFi.begin(WIFI_SSID, WIFI_PASS);`.
Nothing else changes.

`tally/esp/WsTally/secrets.h.example`:
```c
// Copy to secrets.h (git-ignored) and fill in the venue tally WiFi.
#define WIFI_SSID "venue-tally-ssid"
#define WIFI_PASS "venue-tally-password"
```
`.gitignore`: add the line `tally/esp/WsTally/secrets.h`.
`tally/esp/README.md`: what the firmware is (ESP8266 + NeoPixel ring 7, DIP id = camera number), how to build it (Arduino IDE, ESP8266 core, libraries `WebSocketsClient` by Markus Sattler and `Adafruit NeoPixel`), `cp secrets.h.example secrets.h`, the protocol table from the spec, and that the server is `strih-tally` on the strih (issue 1417).

- [ ] **Step 4: Run it** — same command → PASS. Also `git status --porcelain tally/` shows no `secrets.h`.

- [ ] **Step 5: Commit** — `git add tally/esp .gitignore tests/python/test_strih_tally_firmware_1417.py && git commit -m "feat(#1417): the tally ESP firmware in the repo, WiFi credentials in a git-ignored secrets.h" -- tally/esp .gitignore tests/python/test_strih_tally_firmware_1417.py`

---

### Task 2: The pure decision module

**Files:**
- Create: `scripts/strih_tally_decision.py`
- Test: `tests/python/test_strih_tally_decision_1417.py`

**Interfaces — Produces:**
- `Item = dict` with keys `name: str`, `kind: "scene" | "group" | "input"`, `enabled: bool`.
- `camera_of_ndi_name(ndi_name: str | None) -> int` — `CAM<n> (...)` → n, else 0.
- `cameras_in(scene: str, tree: dict[str, list[Item]], cameras: dict[str, int]) -> set[int]` — `tree` maps a scene OR group name to its items; `cameras` maps an input name to its camera (only camera inputs present).
- `camera_of_scene(scene: str | None, tree, cameras) -> int` — exactly one camera → it, else 0.
- `State = tuple[int, int]` = `(pgm, pvw)`.
- `state_of(program: str | None, preview: str | None, studio: bool, tree, cameras) -> State`.
- `messages(prev: State | None, new: State) -> list[str]`.
- `full_messages(state: State) -> list[str]` — `["TALLY-PVW <pvw>", "TALLY-PGM <pgm>"]`.

- [ ] **Step 1: Write the failing tests**

```python
"""issue 1417: scene -> camera for the tally lights (owner rule: exactly one camera lights it)."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "scripts"))
import strih_tally_decision as d  # noqa: E402


def item(name, kind="input", enabled=True):
    return {"name": name, "kind": kind, "enabled": enabled}


CAMERAS = {f"NDI cam{n}": n for n in range(1, 8)}
TREE = {
    **{f"Cam {n}": [item(f"NDI cam{n}"), item("ASIO zvuk")] for n in range(1, 8)},
    "Moderatori": [item("NDI cam3"), item("ASIO zvuk"), item("Odpocet")],
    "MULTIVIEW": [item("NDI 2ME PGM"), item("NDI cam1"), item("NDI cam2"), item("NDI cam5")],
    "Interkom": [item("Ableset"), item("NDI 2ME PVW"), item("Camera info", "scene")],
    "Camera info": [item("Browser camera crew")],
    "CG bridge": [item("CG-obs", enabled=False), item("CG-presenter")],
    "Development": [item("Cam 5", "scene")],
    "Grading": [item("Cam 1", "scene", enabled=False), item("Cam 2", "scene")],
    "Grouped": [item("G1", "group")],
    "G1": [item("NDI cam6")],
    "Loop A": [item("Loop B", "scene")],
    "Loop B": [item("Loop A", "scene"), item("NDI cam4")],
    "Twice": [item("NDI cam3"), item("NDI cam3")],
    "Same cam two inputs": [item("NDI cam3"), item("NDI cam3 copy")],
}


def test_camera_of_ndi_name():
    assert d.camera_of_ndi_name("CAM3 (usb)") == 3
    assert d.camera_of_ndi_name("CAM12 (usb)") == 12
    assert d.camera_of_ndi_name("STRIH-LX (2ME PGM)") == 0
    assert d.camera_of_ndi_name("RESOLUME-SNV (SP-program)") == 0
    assert d.camera_of_ndi_name(None) == 0
    assert d.camera_of_ndi_name("cam3 (usb)") == 0


def test_cam_n_scene():
    for n in range(1, 8):
        assert d.camera_of_scene(f"Cam {n}", TREE, CAMERAS) == n


def test_single_camera_composite_scene():
    assert d.camera_of_scene("Moderatori", TREE, CAMERAS) == 3


def test_multi_camera_scene_lights_nothing():
    assert d.camera_of_scene("MULTIVIEW", TREE, CAMERAS) == 0


def test_no_camera_scene_lights_nothing():
    assert d.camera_of_scene("Interkom", TREE, CAMERAS) == 0
    assert d.camera_of_scene("CG bridge", TREE, CAMERAS) == 0


def test_nested_scene_camera_found():
    assert d.camera_of_scene("Development", TREE, CAMERAS) == 5


def test_disabled_items_do_not_count():
    assert d.camera_of_scene("Grading", TREE, CAMERAS) == 2


def test_group_contents_count():
    assert d.camera_of_scene("Grouped", TREE, CAMERAS) == 6


def test_scene_cycle_terminates():
    assert d.camera_of_scene("Loop A", TREE, CAMERAS) == 4


def test_same_camera_twice_is_one_camera():
    cams = {**CAMERAS, "NDI cam3 copy": 3}
    assert d.camera_of_scene("Twice", TREE, cams) == 3
    assert d.camera_of_scene("Same cam two inputs", TREE, cams) == 3


def test_unknown_or_none_scene():
    assert d.camera_of_scene("No such scene", TREE, CAMERAS) == 0
    assert d.camera_of_scene(None, TREE, CAMERAS) == 0


def test_state_of_studio_off_has_no_preview():
    assert d.state_of("Cam 3", "Cam 5", False, TREE, CAMERAS) == (3, 0)
    assert d.state_of("Cam 3", "Cam 5", True, TREE, CAMERAS) == (3, 5)


def test_messages_order_and_noop():
    assert d.messages(None, (3, 5)) == ["TALLY-PVW 5", "TALLY-PGM 3"]
    assert d.messages((3, 5), (3, 5)) == []
    assert d.messages((3, 5), (5, 3)) == ["TALLY-PVW 3", "TALLY-PGM 5"]
    assert d.messages((3, 5), (3, 2)) == ["TALLY-PVW 2"]
    assert d.messages((3, 5), (4, 5)) == ["TALLY-PGM 4"]
    assert d.full_messages((0, 0)) == ["TALLY-PVW 0", "TALLY-PGM 0"]
```

- [ ] **Step 2: Run** — `python3 -m pytest -q tests/python/test_strih_tally_decision_1417.py` → FAIL (module missing).

- [ ] **Step 3: Implement `scripts/strih_tally_decision.py`**

```python
"""issue 1417: which camera a strih scene lights on the tally lights. Pure, no I/O.

Owner rule (12.10.2026): a scene lights camera n when its enabled content, nested scenes and groups
included, holds exactly ONE distinct camera n; anything else (no camera, several) lights nothing (0).
A camera is identified by the NDI source name `CAM<n> (...)` of an ndi_source input.
"""
import re

_CAM = re.compile(r"^CAM(\d+) \(")


def camera_of_ndi_name(ndi_name):
    m = _CAM.match(ndi_name or "")
    return int(m.group(1)) if m else 0


def cameras_in(scene, tree, cameras, _seen=None):
    seen = set() if _seen is None else _seen
    if scene is None or scene in seen:
        return set()
    seen.add(scene)
    found = set()
    for it in tree.get(scene, []):
        if not it.get("enabled", True):
            continue
        if it["kind"] in ("scene", "group"):
            found |= cameras_in(it["name"], tree, cameras, seen)
        elif cameras.get(it["name"], 0):
            found.add(cameras[it["name"]])
    return found


def camera_of_scene(scene, tree, cameras):
    found = cameras_in(scene, tree, cameras)
    return next(iter(found)) if len(found) == 1 else 0


def state_of(program, preview, studio, tree, cameras):
    pvw = camera_of_scene(preview, tree, cameras) if studio else 0
    return (camera_of_scene(program, tree, cameras), pvw)


def full_messages(state):
    pgm, pvw = state
    return [f"TALLY-PVW {pvw}", f"TALLY-PGM {pgm}"]


def messages(prev, new):
    if prev is None:
        return full_messages(new)
    out = []
    if prev[1] != new[1]:
        out.append(f"TALLY-PVW {new[1]}")
    if prev[0] != new[0]:
        out.append(f"TALLY-PGM {new[0]}")
    return out
```

Note `cameras_in` shares one `seen` set across siblings on purpose: a scene reached twice is counted once (same camera set), and a cycle stops.

- [ ] **Step 4: Run** — same command → PASS.

- [ ] **Step 5: Commit** — `git commit -m "feat(#1417): strih_tally_decision -- scene to camera for the tally lights" -- scripts/strih_tally_decision.py tests/python/test_strih_tally_decision_1417.py` (add both first).

---

### Task 3: The service

**Files:**
- Create: `scripts/strih_tally.py`
- Test: `tests/python/test_strih_tally_service_1417.py`

**Interfaces:**
- Consumes: Task 2 (`state_of`, `messages`, `full_messages`, `camera_of_ndi_name`).
- Produces: CLI `python3 strih_tally.py --port PORT --obs URL [--resend-s 10] [--obs-loss-s 10] [--retry-s 2] [--host 0.0.0.0]`; `GET /state` → JSON `{"pgm": int, "pvw": int, "program_scene": str|null, "preview_scene": str|null, "studio_mode": bool, "obs_connected": bool, "tallies": [str]}` (`tallies` = `"<ip> <JOIN name>"` per connected client that sent JOIN).

Design (implement exactly this shape):
- `class Tally` holds `clients: set`, `joined: dict[conn, str]`, `snap` (program, preview, studio, tree, cameras), `state: tuple|None`, `obs_connected: bool`, `last_obs_ok: float`, `gray_sent: bool`.
- `async def send_all(texts)` — sends each text to every client; a send failure on one client is logged and that client dropped.
- Relay handler `async def handler(conn)`: add to `clients`; send `full_messages(state or (0, 0))` to it; for each received text: strip; if it starts with `JOIN ` remember the name and log `JOIN <name> <ip>`; forward the text to every other client; on close remove + log `LEAVE`.
- `process_request(conn, request)`: if `request.path == "/state"` return `conn.respond(200, json.dumps(state_json()))`; else return None (WS handshake proceeds).
- OBS loop `async def obs_loop()`: forever: connect (`websockets.asyncio.client.connect(url, max_size=None)`), read Hello (op 0), send Identify `{"op":1,"d":{"rpcVersion":1,"eventSubscriptions":1180}}` (Scenes 4 + Inputs 8 + Transitions 16 + SceneItems 128 + Ui 1024), read Identified (op 2); `await refresh()`; then for each event (op 5): `SceneTransitionStarted` → `send_all(["TALLY-TRS"])`; any of `CurrentProgramSceneChanged`, `CurrentPreviewSceneChanged`, `StudioModeStateChanged`, `SceneItemCreated`, `SceneItemRemoved`, `SceneItemEnableStateChanged`, `SceneListChanged`, `InputSettingsChanged`, `InputNameChanged`, `SceneTransitionEnded` → `await refresh()`. On any exception: log `OBS disconnected: <err>`, `obs_connected = False`, sleep `retry_s`, loop.
- `async def refresh()`: one request at a time over the same connection (a request id counter; read frames until the op 7 with that id; any op 5 event read meanwhile is queued and handled after). Requests: `GetStudioModeEnabled`, `GetCurrentProgramScene`, `GetCurrentPreviewScene` (only when studio on; a failure → preview None), `GetSceneList`, `GetSceneItemList` per scene, `GetGroupSceneItemList` per item with `isGroup`, `GetInputList`, `GetInputSettings` per `ndi_source` input (`ndi_source_name`). Build `tree` items: `kind = "group" if isGroup else ("scene" if sourceType == "OBS_SOURCE_TYPE_SCENE" else "input")`, `enabled = sceneItemEnabled`. `cameras = {inputName: camera_of_ndi_name(ndi_source_name)}` for non-zero ones. New state = `state_of(...)`; `send_all(messages(self.state, new))`; log `pgm a->b pvw c->d (program=<scene> preview=<scene>)` only when changed; set `obs_connected=True`, `last_obs_ok=now`, `gray_sent=False`.
- Resend loop: every `resend_s` → `send_all(full_messages(state))` when `state` is not None.
- Fallback loop: every 1 s: if not `obs_connected` and `now - last_obs_ok > obs_loss_s` and not `gray_sent` → `state=(0,0)`, `send_all(full_messages((0,0)))`, log `OBS lost > Ns: tally gray`, `gray_sent=True`.
- `main()`: argparse, logging to stderr with `%(asctime)s %(message)s`, `asyncio.run` of `serve(handler, host, port, process_request=...)` + the three loops.

- [ ] **Step 1: Write the failing tests** (a fake OBS = a `websockets` server on 127.0.0.1:0 speaking the v5 subset above from a fixture; fake tallies = `websockets` clients)

```python
"""issue 1417: strih-tally end to end against a fake OBS and fake tally clients (no rig)."""
import asyncio
import json
import os
import sys
import urllib.request

import pytest
from websockets.asyncio.client import connect
from websockets.asyncio.server import serve

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "scripts"))
import strih_tally  # noqa: E402

class FakeObs:
    def __init__(self):
        self.program, self.preview, self.studio = "Cam 3", "Cam 5", True
        self.scenes = {f"Cam {n}": [(f"NDI cam{n}", "input")] for n in range(1, 8)}
        self.scenes["MULTIVIEW"] = [("NDI cam1", "input"), ("NDI cam2", "input")]
        self.inputs = {f"NDI cam{n}": f"CAM{n} (usb)" for n in range(1, 8)}
        self.conns = set()

    def answer(self, rt, data):
        if rt == "GetStudioModeEnabled":
            return {"studioModeEnabled": self.studio}
        if rt == "GetCurrentProgramScene":
            return {"currentProgramSceneName": self.program, "sceneName": self.program}
        if rt == "GetCurrentPreviewScene":
            return {"currentPreviewSceneName": self.preview, "sceneName": self.preview}
        if rt == "GetSceneList":
            return {"scenes": [{"sceneName": s} for s in self.scenes]}
        if rt == "GetSceneItemList":
            return {"sceneItems": [{"sourceName": n, "isGroup": False, "sceneItemEnabled": True,
                                    "sourceType": "OBS_SOURCE_TYPE_SCENE" if k == "scene" else "OBS_SOURCE_TYPE_INPUT"}
                                   for n, k in self.scenes.get(data["sceneName"], [])]}
        if rt == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": "ndi_source"} for n in self.inputs]}
        if rt == "GetInputSettings":
            return {"inputSettings": {"ndi_source_name": self.inputs[data["inputName"]]}}
        return {}

    async def handler(self, ws):
        self.conns.add(ws)
        await ws.send(json.dumps({"op": 0, "d": {"rpcVersion": 1}}))
        await ws.recv()
        await ws.send(json.dumps({"op": 2, "d": {"negotiatedRpcVersion": 1}}))
        try:
            async for raw in ws:
                m = json.loads(raw)
                if m["op"] == 6:
                    d = m["d"]
                    await ws.send(json.dumps({"op": 7, "d": {"requestType": d["requestType"], "requestId": d["requestId"],
                                                             "requestStatus": {"result": True, "code": 100},
                                                             "responseData": self.answer(d["requestType"], d.get("requestData", {}))}}))
        finally:
            self.conns.discard(ws)

    async def event(self, name, data=None):
        for ws in list(self.conns):
            await ws.send(json.dumps({"op": 5, "d": {"eventType": name, "eventData": data or {}}}))


async def start(fake, **kw):
    obs = await serve(fake.handler, "127.0.0.1", 0)
    obs_url = "ws://127.0.0.1:%d" % obs.sockets[0].getsockname()[1]
    t = strih_tally.Tally(obs_url, resend_s=kw.get("resend_s", 10), obs_loss_s=kw.get("obs_loss_s", 10), retry_s=0.1)
    srv = await t.start_server("127.0.0.1", 0)
    port = srv.sockets[0].getsockname()[1]
    return obs, t, srv, port


async def recv_until(ws, want, timeout=3):
    got = []
    async def _r():
        while want not in got:
            got.append(await ws.recv())
    await asyncio.wait_for(_r(), timeout)
    return got


def run(coro):
    return asyncio.run(coro)


def test_initial_state_and_program_change():
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await tally.send("JOIN ESP-3")
            await recv_until(tally, "TALLY-PGM 3")
            fake.program = "Cam 4"
            await fake.event("CurrentProgramSceneChanged", {"sceneName": "Cam 4"})
            await recv_until(tally, "TALLY-PGM 4")
        await t.stop(); srv.close(); obs.close()
    run(go())


def test_tally_joining_before_obs_gets_zeros():
    async def go():
        t = strih_tally.Tally("ws://127.0.0.1:9", resend_s=10, obs_loss_s=10, retry_s=0.1)
        srv = await t.start_server("127.0.0.1", 0)
        port = srv.sockets[0].getsockname()[1]
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            got = await recv_until(tally, "TALLY-PGM 0")
            assert got[:2] == ["TALLY-PVW 0", "TALLY-PGM 0"]
        await t.stop(); srv.close()
    run(go())


def test_preview_change():
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await recv_until(tally, "TALLY-PGM 3")
            fake.preview = "Cam 2"
            await fake.event("CurrentPreviewSceneChanged", {"sceneName": "Cam 2"})
            await recv_until(tally, "TALLY-PVW 2")
            assert t.state == (3, 2)
        await t.stop(); srv.close(); obs.close()
    run(go())


def test_transition_sends_trs():
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await recv_until(tally, "TALLY-PGM 3")
            await fake.event("SceneTransitionStarted", {"transitionName": "Fade"})
            await recv_until(tally, "TALLY-TRS")
        await t.stop(); srv.close(); obs.close()
    run(go())


def test_studio_off_preview_zero_and_multiview_zero():
    async def go():
        fake = FakeObs()
        fake.studio = False
        fake.program = "MULTIVIEW"
        obs, t, srv, port = await start(fake)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            got = await recv_until(tally, "TALLY-PGM 0")
            assert "TALLY-PVW 0" in got
        await t.stop(); srv.close(); obs.close()
    run(go())


def test_relay_forwards_to_others_only():
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake)
        async with connect(f"ws://127.0.0.1:{port}/") as a, connect(f"ws://127.0.0.1:{port}/") as b:
            await recv_until(a, "TALLY-PGM 3"); await recv_until(b, "TALLY-PGM 3")
            await a.send("SET-LUMINANCE 120")
            await recv_until(b, "SET-LUMINANCE 120")
            with pytest.raises(asyncio.TimeoutError):
                await asyncio.wait_for(a.recv(), 0.5)
            assert t.state == (3, 5)
        await t.stop(); srv.close(); obs.close()
    run(go())


def test_resend_every_interval():
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake, resend_s=0.3)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await recv_until(tally, "TALLY-PGM 3")
            later = await recv_until(tally, "TALLY-PGM 3", timeout=2)
            assert later[-2:] == ["TALLY-PVW 5", "TALLY-PGM 3"]
        await t.stop(); srv.close(); obs.close()
    run(go())


def test_obs_loss_goes_gray_once():
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake, obs_loss_s=0.3)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await recv_until(tally, "TALLY-PGM 3")
            obs.close()
            for ws in list(fake.conns):
                await ws.close()
            await recv_until(tally, "TALLY-PGM 0", timeout=4)
            assert t.state == (0, 0)
        await t.stop(); srv.close()
    run(go())


def test_obs_reconnect_sends_new_state():
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake)
        obs_port = obs.sockets[0].getsockname()[1]
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await recv_until(tally, "TALLY-PGM 3")
            for ws in list(fake.conns):
                await ws.close()
            obs.close()
            fake.program = "Cam 6"
            obs2 = await serve(fake.handler, "127.0.0.1", obs_port)
            await recv_until(tally, "TALLY-PGM 6", timeout=4)
            obs2.close()
        await t.stop(); srv.close()
    run(go())


def test_state_endpoint():
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await tally.send("JOIN ESP-3")
            await recv_until(tally, "TALLY-PGM 3")
            body = await asyncio.to_thread(lambda: urllib.request.urlopen(f"http://127.0.0.1:{port}/state", timeout=2).read())
            st = json.loads(body)
            assert st["pgm"] == 3 and st["pvw"] == 5 and st["obs_connected"] is True
            assert st["program_scene"] == "Cam 3" and any(x.endswith(" ESP-3") for x in st["tallies"])
        await t.stop(); srv.close(); obs.close()
    run(go())
```

The tests use `asyncio.run`, no pytest-asyncio plugin. The reconnect test re-binds the same port; if the OS refuses (TIME_WAIT), bind the first fake with `reuse_address=True`.

- [ ] **Step 2: Run** — `python3 -m pytest -q tests/python/test_strih_tally_service_1417.py` → FAIL (module missing). Needs `python3-websockets` on dev1 (`python3 -c "import websockets"`); if missing, `pip install --user websockets==15.0.1` (the Ubuntu 26.04 version) — never vendor it. CI: add `websockets==15.0.1` to the python-tests job's pip install line in `.github/workflows/ci.yml` if that job installs packages explicitly (read the job first).

- [ ] **Step 3: Implement `scripts/strih_tally.py`** to the design above. Public surface used by the tests: `Tally(obs_url, resend_s, obs_loss_s, retry_s)`, `await Tally.start_server(host, port) -> Server` (starts the server AND the three loops as tasks), `Tally.state`, `await Tally.stop()` (cancels the loops, closes the OBS connection), `main()`. Start the file with a module docstring that names issue 1417, the spec path and the protocol.

- [ ] **Step 4: Run** — same command → PASS, run it 3 times (no flakes). Then the decision tests again.

- [ ] **Step 5: Commit** — `git commit -m "feat(#1417): strih-tally service -- the :9240 relay, the OBS client, GET /state" -- scripts/strih_tally.py tests/python/test_strih_tally_service_1417.py` (plus ci.yml if touched).

---

### Task 4: Install + grade on the strih boxes

**Files:**
- Create: `systemd/strih-tally.service.in`, `systemd/strih-tally-mdns.service`, `scripts/lib/strih-tally.sh`
- Modify: `scripts/lib/obs-box-role.sh`, `scripts/lib/strih-provision.sh`, `scripts/setup-strih.sh`, `scripts/verify-strih.sh`
- Test: `tests/python/test_strih_tally_install_1417.py`

**Interfaces:**
- Consumes: Task 3 CLI and `/state` JSON.
- Produces: `strih_tally_install REPO DESKTOP_USER` (rc 0/1, prints one line per action); `strih_tally_grade_rows REPO DESKTOP_USER` (rows `OK|...`, `FAIL|...`, `NOTE|...`), `strih_tally_row_count` (= 4), `strih_tally_grade_report REPO DESKTOP_USER` (through the caller's `ok`/`note`/`bad`, rc 1 unless all rows printed). Overridable seams for tests: `STRIH_TALLY_PREFIX` (default `/usr/local/lib/camera-box`), `STRIH_TALLY_UNIT_DIR` (default `/etc/systemd/system`), `STRIH_TALLY_DEFAULT_FILE` (default `/etc/default/strih-tally`), `STRIH_TALLY_SYSTEMCTL` (default `systemctl`), `STRIH_TALLY_STATE_URL` (default `http://127.0.0.1:9240/state`), `STRIH_TALLY_RESOLVE_CMD` (default `avahi-resolve -4 -n atemrpi.local`), `STRIH_TALLY_OBS_PROGRAM_CMD` (default: a `python3 -c` one-liner with `websocket` (websocket-client, already on the box) printing `GetCurrentProgramScene`'s `currentProgramSceneName` from `ws://127.0.0.1:4455`).

Unit `systemd/strih-tally.service.in`:
```ini
[Unit]
Description=Camera tally relay + strih OBS client (issue 1417)
After=network-online.target
Wants=network-online.target

[Service]
User=@USER@
ExecStart=/usr/bin/python3 /usr/local/lib/camera-box/strih_tally.py --port 9240 --obs ws://127.0.0.1:4455
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
```
Unit `systemd/strih-tally-mdns.service`:
```ini
[Unit]
Description=Announce atemrpi.local for the camera tally lights (issue 1417)
After=avahi-daemon.service network-online.target
Requires=avahi-daemon.service

[Service]
EnvironmentFile=/etc/default/strih-tally
ExecStart=/usr/bin/avahi-publish -a -R atemrpi.local ${STRIH_TALLY_IP}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Install (`strih_tally_install`), in order, every write through `strih_session_apps_put SRC DST MODE` (prints `written|unchanged`; read it in `scripts/lib/strih-session-apps.sh` first):
1. `apt-get install -y python3-websockets avahi-utils` only when missing (`dpkg -s`), fail loud.
2. `strih_tally.py` + `strih_tally_decision.py` → `$PREFIX`, mode 0644.
3. Render `strih-tally.service.in` with `@USER@` = DESKTOP_USER into a temp file, put → `$UNIT_DIR/strih-tally.service`; put `strih-tally-mdns.service`.
4. Write `$DEFAULT_FILE` = `STRIH_TALLY_IP=<strih_box_fact STRIH_IP>` (0644), through the same put.
5. `systemctl daemon-reload` when anything was written; `systemctl enable --now` both units (this step is not enable-only: tally is production function with no TEST/EVENT dependency); `try-restart` a unit whose files changed.

Grade rows (item 42), exactly 4:
1. `(tally-units)` both units enabled + active.
2. `(tally-port)` `GET $STATE_URL` answers JSON with `obs_connected: true`.
3. `(tally-mdns)` `$RESOLVE_CMD` prints exactly one address and it equals STRIH_IP (another device answering `atemrpi.local` = FAIL naming its address).
4. `(tally-obs)` `/state.program_scene` equals `$OBS_PROGRAM_CMD`'s output.

Role gate in `scripts/lib/obs-box-role.sh`: add `16f` to the strih-only case (`3-nvenc | 14 | 16 | 16c | 16f)`) with the message `the stream role runs no camera tally (no camera operators on stream-pp)`, and the table comment line `#   16f     tally            skipped`.

Wiring, within the 1000-line budget:
- `scripts/lib/strih-provision.sh`: source `strih-tally.sh` next to `strih-program-audio.sh` (same guard style).
- `scripts/setup-strih.sh`, right after step 16e:
```bash
# ---------------------------------------------------------------------------------------------
step "16f" "Camera tally (issue 1417): strih-tally relay + OBS client, atemrpi.local -- system units"
if WHY="$(obs_box_step_applies 16f)"; then strih_tally_install "${HERE}/.." "$DESKTOP_USER" || fail "issue 1417: strih-tally could not be provisioned (above)"
else echo "  SKIP: ${WHY}"; fi
```
- `scripts/verify-strih.sh`, right after item 41:
```bash
# 42) issue 1417 camera tally (scripts/lib/strih-tally.sh): units, :9240 /state, atemrpi.local, OBS program.
if WHY="$(obs_box_step_applies 16f)"; then strih_tally_grade_report "${HERE}/.." "${STRIH_LX_USER:-newlevel}" || bad "(tally) the grader did not print all of its rows"
else note "(tally) not graded: ${WHY}"; fi
```
- Make room: move the 4-line comment above step 16e in setup-strih.sh (`# issue 1404 (ROZHODNUTE …` through `# The whole step lives in …`) into the header of `scripts/lib/strih-program-audio.sh` (keep the text, add "setup-strih step 16e:"), and the 2-line comment above item 41 in verify-strih.sh into the same header. Then `wc -l scripts/setup-strih.sh scripts/verify-strih.sh` must print ≤ 1000 each. Before moving, run the old-vs-new literal-count anchor sweep over every `tests/**` file that names either script (CLAUDE.md, the recording-e2e anchor section, applies here too) and confirm no 1→0 or 1→2.

- [ ] **Step 1: Write the failing tests** — `tests/python/test_strih_tally_install_1417.py`: run `strih_tally_install` and `strih_tally_grade_rows` under `bash -c 'set -euo pipefail; . scripts/lib/strih-session-apps.sh; . scripts/lib/strih-box-facts.sh; strih_box_load strih-lx; . scripts/lib/strih-tally.sh; …'` with every seam pointed into `tmp_path` and stub commands on PATH (`systemctl` logging its args, `apt-get`/`dpkg` stubs, `avahi-resolve` printing a fixture). Pass values as positional arguments, never inside the script text. Cases:
  - a first install writes 5 files (two scripts, two units, the default file) with `@USER@` rendered and `STRIH_TALLY_IP=10.77.9.202`, then `daemon-reload` and `enable --now strih-tally.service strih-tally-mdns.service`;
  - a second run writes nothing and runs no daemon-reload;
  - grade: all OK with healthy stubs; `(tally-mdns)` FAIL when the resolver prints a different address or two lines; `(tally-port)` FAIL when /state is unreachable (point `STRIH_TALLY_STATE_URL` at a closed port) or `obs_connected` false (serve a fixture with `python3 -m http.server` on port 0); `(tally-obs)` FAIL when the program scenes differ;
  - `obs_box_step_applies 16f` returns non-zero with the message for `stream-pp`, zero for `strih-lx` and `strih-pp`;
  - `setup-strih.sh` and `verify-strih.sh` call the lib once each and stay ≤ 1000 lines.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** the units, lib, role gate and wiring above.
- [ ] **Step 4: Run** the new test, `tests/python/test_strih_session_apps_1399.py`, `tests/python/test_strih_pp_box_1412.py`, `tests/python/test_obs_box_stream_role_1412.py`, plus `bash -n` and `shellcheck -S warning` on the touched `.sh` files → all PASS.
- [ ] **Step 5: Commit** (pathspec; quoted-heredoc message).

---

### Task 5: Playbook

**Files:** Create `.claude/rules/strih-tally.md` (with `paths:` for `scripts/strih_tally*.py`, `scripts/lib/strih-tally.sh`, `systemd/strih-tally*`, `tally/**`); modify `CLAUDE.md` (one router line under the strih entries).

Content: the protocol table, the owner scene rule, why firmware is unchanged (atemrpi.local + :9240 hard-coded), the relay contract (Companion can still send LUMINANCE/LOCK), the 10-s resend and the gray fallback, install/grade (step 16f / item 42, strih role only), the secrets.h rule, how to check live (`curl -s http://<strih>:9240/state`, `journalctl -u strih-tally`), and that a second device answering `atemrpi.local` on the venue LAN breaks the tallies (item 42 names it).

- [ ] Write both, commit.

---

## Live rollout (supervisor, after the lane is integrated — not a lane step)

1. On strih-pp (free to use): stage the tree, run only `strih_tally_install`, then verify-strih → item 42 OK; `curl /state` while switching scenes in OBS; read `JOIN` lines when a tally powers on.
2. Same on strih-snv after the owner's production, never during one.
3. Ask the owner to power one tally per venue and confirm red/green follow the cut.
