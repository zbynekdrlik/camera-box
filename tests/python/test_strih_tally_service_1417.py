"""issue 1417: strih-tally end to end against a fake OBS and fake tally clients (no rig)."""
import asyncio
import json
import os
import sys
import urllib.request

import pytest
from websockets.asyncio.client import connect
from websockets.asyncio.server import serve
from websockets.exceptions import ConnectionClosed

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "scripts"))
import strih_tally  # noqa: E402


class FakeObs:
    def __init__(self):
        self.program, self.preview, self.studio = "Cam 3", "Cam 5", True
        self.scenes = {f"Cam {n}": [(f"NDI cam{n}", "input")] for n in range(1, 8)}
        self.scenes["MULTIVIEW"] = [("NDI cam1", "input"), ("NDI cam2", "input")]
        self.inputs = {f"NDI cam{n}": f"CAM{n} (usb)" for n in range(1, 8)}
        self.disabled = set()  # (scene, item name) pairs whose sceneItemEnabled is false
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
            return {"sceneItems": [{"sourceName": n, "isGroup": False,
                                    "sceneItemEnabled": (data["sceneName"], n) not in self.disabled,
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


ME2_TWINS = ("TALLY-PVW2 ", "TALLY-PGM2 ")


async def recv_until(ws, want, timeout=3, me2=False):
    """The frames up to `want`. The 2ME twins (owner 14.10.2026, the PP lights) are left out unless
    me2=True; test_every_frame_goes_out_in_both_forms pins them."""
    got = []
    async def _r():
        while want not in got:
            msg = await ws.recv()
            if me2 or not msg.startswith(ME2_TWINS):
                got.append(msg)
        cmd, _, arg = want.partition(" ")
        if not me2 and cmd in ("TALLY-PVW", "TALLY-PGM"):
            twin = await ws.recv()   # the 2ME twin follows its plain frame at once
            assert twin == f"{cmd}2 {arg}", twin
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

# --- review round 1 (issue 1417): transitions, OBS drops, gray once, remapping, relay limits -----------

async def recv_for(ws, seconds):
    """Everything the tally receives within `seconds` (a quiet period ends it, never an error)."""
    got = []
    async def _r():
        while True:
            msg = await ws.recv()
            if not msg.startswith(ME2_TWINS):
                got.append(msg)
    try:
        await asyncio.wait_for(_r(), seconds)
    except asyncio.TimeoutError:
        pass
    return got


def test_a_transition_keeps_both_cameras_red_then_restores_the_normal_colours():
    """TALLY-TRS must follow the program change (the firmware recolours on TALLY-PGM), and the end of the
    transition must send the full state again so the lights leave the TRS colours."""
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await recv_until(tally, "TALLY-PGM 3")
            await fake.event("SceneTransitionStarted", {"transitionName": "Fade"})
            await recv_until(tally, "TALLY-TRS")
            fake.program = "Cam 4"
            await fake.event("CurrentProgramSceneChanged", {"sceneName": "Cam 4"})
            got = await recv_until(tally, "TALLY-PGM 4")
            got += await recv_for(tally, 0.5)
            assert got[got.index("TALLY-PGM 4") + 1:] == ["TALLY-TRS"], got
            await fake.event("SceneTransitionEnded", {"transitionName": "Fade"})
            end = await recv_until(tally, "TALLY-PGM 4")
            assert end[-2:] == ["TALLY-PVW 5", "TALLY-PGM 4"], end
        await t.stop(); srv.close(); obs.close()
    run(go())


def test_a_short_obs_drop_keeps_the_state():
    """OBS gone for 0.8 s (< obs_loss_s 1.5): several fallback ticks see it gone, none may go gray."""
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake, obs_loss_s=1.5)
        obs_port = obs.sockets[0].getsockname()[1]
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await recv_until(tally, "TALLY-PGM 3")
            obs.close()
            for ws in list(fake.conns):
                await ws.close()
            got = await recv_for(tally, 0.8)
            obs2 = await serve(fake.handler, "127.0.0.1", obs_port)
            got += await recv_for(tally, 1.5)
            assert "TALLY-PGM 0" not in got, got
            assert t.state == (3, 5) and t.obs_connected
            obs2.close()
        await t.stop(); srv.close()
    run(go())


def test_a_resend_during_a_transition_never_resends_the_program():
    """The firmware shifts its stored program into trs on EVERY TALLY-PGM: a mid-fade resend must send
    only TALLY-PVW + TALLY-TRS, or the outgoing camera loses its red for the rest of the fade."""
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake, resend_s=0.3)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await recv_until(tally, "TALLY-PGM 3")
            await fake.event("SceneTransitionStarted", {"transitionName": "Fade"})
            fake.program = "Cam 4"
            await fake.event("CurrentProgramSceneChanged", {"sceneName": "Cam 4"})
            got = await recv_until(tally, "TALLY-PGM 4")
            got += await recv_for(tally, 1.2)
            mid = got[got.index("TALLY-PGM 4") + 1:]
            assert not any(x.startswith("TALLY-PGM") for x in mid), mid
            assert mid.count("TALLY-PVW 5") >= 2, mid
            await fake.event("SceneTransitionEnded", {"transitionName": "Fade"})
            end = await recv_until(tally, "TALLY-PGM 4")
            assert end[-2:] == ["TALLY-PVW 5", "TALLY-PGM 4"], end
        await t.stop(); srv.close(); obs.close()
    run(go())


def test_obs_loss_sends_gray_exactly_once():
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake, obs_loss_s=0.3, resend_s=30)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await recv_until(tally, "TALLY-PGM 3")
            obs.close()
            for ws in list(fake.conns):
                await ws.close()
            got = await recv_for(tally, 2.0)
            assert got.count("TALLY-PGM 0") == 1 and got.count("TALLY-PVW 0") == 1, got
        await t.stop(); srv.close()
    run(go())


def test_a_scene_item_event_remaps_the_camera():
    async def go():
        fake = FakeObs()
        fake.program = "MULTIVIEW"
        obs, t, srv, port = await start(fake)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await recv_until(tally, "TALLY-PGM 0")
            fake.disabled.add(("MULTIVIEW", "NDI cam2"))
            await fake.event("SceneItemEnableStateChanged", {"sceneName": "MULTIVIEW", "sceneItemEnabled": False})
            await recv_until(tally, "TALLY-PGM 1")
        await t.stop(); srv.close(); obs.close()
    run(go())


def test_the_relay_forwards_only_protocol_texts():
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake)
        async with connect(f"ws://127.0.0.1:{port}/") as a, connect(f"ws://127.0.0.1:{port}/") as b:
            await recv_until(a, "TALLY-PGM 3"); await recv_until(b, "TALLY-PGM 3")
            await a.send("HELLO everyone")
            await a.send("TALLY-LOCK 1")
            got = await recv_until(b, "TALLY-LOCK 1")
            assert "HELLO everyone" not in got, got
        await t.stop(); srv.close(); obs.close()
    run(go())


def test_an_oversized_frame_closes_its_sender_and_reaches_nobody():
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake)
        async with connect(f"ws://127.0.0.1:{port}/") as a, connect(f"ws://127.0.0.1:{port}/") as b:
            await recv_until(a, "TALLY-PGM 3"); await recv_until(b, "TALLY-PGM 3")
            await a.send("SET-LUMINANCE " + "9" * 4000)
            got = await recv_for(b, 0.5)
            assert not any(x.startswith("SET-LUMINANCE") for x in got), got
            with pytest.raises(ConnectionClosed):
                for _ in range(10):
                    await asyncio.wait_for(a.recv(), 2)
        await t.stop(); srv.close(); obs.close()
    run(go())


def test_state_reports_the_mapped_cameras_and_clean_join_names():
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            await tally.send("JOIN ESP-3\nforged line\x1b[31m")
            await recv_until(tally, "TALLY-PGM 3")
            await asyncio.sleep(0.2)
            body = await asyncio.to_thread(lambda: urllib.request.urlopen(f"http://127.0.0.1:{port}/state", timeout=2).read())
            st = json.loads(body)
            assert st["camera_inputs"] == 7
            assert len(st["tallies"]) == 1
            name = st["tallies"][0]
            assert "\n" not in name and "\x1b" not in name and "ESP-3" in name, name
        await t.stop(); srv.close(); obs.close()
    run(go())



def test_every_frame_goes_out_in_both_forms():
    """Owner 14.10.2026: the Poprad lights read TALLY-PVW2 / TALLY-PGM2. Each PVW / PGM frame is
    followed by its 2ME twin, so preview still precedes program in both forms."""
    async def go():
        fake = FakeObs()
        obs, t, srv, port = await start(fake)
        async with connect(f"ws://127.0.0.1:{port}/") as tally:
            got = await recv_until(tally, "TALLY-PGM2 3", me2=True)
            assert got[-4:] == ["TALLY-PVW 5", "TALLY-PVW2 5", "TALLY-PGM 3", "TALLY-PGM2 3"], got
        await t.stop(); srv.close(); obs.close()
    run(go())
