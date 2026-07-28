#!/usr/bin/env python3
"""imag-nb OBS profile + scene seeding over WebSocket (#458, #501).

Idempotent — CreateScene/CreateInput "already exists" errors are ignored, settings are
re-applied on every run (self-healing, same philosophy as the genlock lockdown).

Seeds (spec docs/superpowers/specs/2026-07-03-imag-nb-topology-design.md, Phase 1):
  - video: 1920x1080 canvas+output @ 60fps
  - scenes "Cam 1".."Cam <IMAG_SCENE_CAM_COUNT>" (env-overridable, default 7 -- #791), each with
    one NDI input "NDI CAM<n>" -> "CAM<n> (usb)"
    (low-latency mode, muted audio, bounds-scaled to fill the canvas) — FULL-bandwidth,
    what the Stream Deck cuts to program.
  - scenes "MV Cam 1".."MV Cam <IMAG_SCENE_CAM_COUNT>" (#501), each with one NDI input "MV CAM<n>" bound to the
    SAME fleet NDI name but flagged genlock_monitor=true, so the vendor/distroav genlock
    lockdown forces LOW-bandwidth NDI receive (~9x cheaper) for these monitor-only twins.
    Root cause (issue #501, runtime-proven with an eglSwapBuffers-counting shim): the
    built-in 6-cell multiview costs ~80ms/render on imag's Linux/OpenGL build because every
    cell synchronously uploads ALL 6 cameras' FULL-1080p NDI textures (their async upload
    otherwise only happens when something renders them). Feeding the multiview from these
    low-bandwidth twins instead fits the #276/#278/#293 render-budget decouple back inside
    the 16.6ms tick.
  - built-in OBS multiview membership (per-scene `show_in_multiview` private setting, set
    via obs-websocket `SetSourcePrivateSettings` — mirrors OBSBasic_Scenes.cpp's
    "ShowInMultiview" context-menu action, which reads/writes the SAME key on
    `obs_source_get_private_settings(sceneSource)`): the 6 "MV Cam N" twins are shown, the 6
    real "Cam N" scenes (and everything else) are hidden — the cutter's Stream Deck still
    cuts the real full-bw scenes to program, the built-in multiview only ever renders the
    cheap low-bw twins.
  - Studio Mode ON, program parked on "Cam 1"
  --projector: fullscreen PROGRAM projector on the HDMI monitor + built-in MULTIVIEW projector
    on the panel (#522/#488 — self-healed every boot by the openbox autostart hook)

Usage:
  imag_scenes.py --host 10.77.9.187 [--password PW] [--projector]

--host has no default (always required) -- it is just an example above. The rig's own harness
scripts resolve the CURRENT active imag host from scripts/imag-host.sh (#832; single source of
truth, reversible between the incumbent 10.77.9.182 and the replacement 10.77.9.187) rather than
hardcoding either address, so a future swap never means hunting a literal here too.
"""

import argparse
import base64
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys

# #785: --bootstrap = boot/recovery invocation (autostart + watchdog) — ONLY that path may
# enforce program scene / studio / input bindings; a bare run only creates what is missing.
BOOTSTRAP = "--bootstrap" in sys.argv

from websocket import create_connection

# #526: VERIFIED physical camera <-> NDI-name mapping (live-checked 2026-07-05, all 6 boxes up;
# cam7 added #753/#791, box 10.77.9.67 -> "CAM7 (usb)"). The fleet advertises a clean 1:1 by box
# number: box 10.77.9.61 -> "CAM1 (usb)", .62 -> "CAM2", .63 -> "CAM3", .64 -> "CAM4",
# .65 -> "CAM5", .66 -> "CAM6", .67 -> "CAM7". So the naive 1:1 below ("MV Cam n" / "Cam n" bound
# to "CAMn (usb)") IS the intended physical order, and the built-in multiview tile order (= scene
# list order MV Cam 1..N) matches the physical camera numbering the cutter expects. This differs
# from strih's OBS-source LABEL offset (that offset is in strih's source naming, not in the NDI
# sender names, which are 1:1 to box number on every box). Pinned by a guard test in
# tests/harness_imag_topology.rs so a silent reorder can't drift it.
#
# #791: this used to be a bare hardcoded `range(1, 7)` (1..6) -- when a 7th camera (cam7,
# 10.77.9.67, #753) was wired into the fleet, imag's OWN scene seeder never grew to include it
# (the literal range excluded it silently, and the boot-time --bootstrap self-heal therefore never
# created/repaired "Cam 7"/"MV Cam 7" or enforced their Multiview membership). Never hardcode this
# count again -- IMAG_SCENE_CAM_COUNT is the ONE declared, env-overridable value, so a future
# fleet growth to cam8 needs no code edit here (mirrors the single-source-of-truth philosophy of
# CAMERA_ACTIVE_SET in scripts/camera-set.sh / .claude/rules/camera-active-set.md -- though this is
# imag's OWN scene-camera count, independent of that E2E active-fleet list: imag has always seeded
# Cam5/Cam6 scenes even while CAMERA_ACTIVE_SET retired those boxes from the E2E zero-loss sweep,
# since imag's Multiview/cut preview is a different concern from the recording-verdict test fleet).
IMAG_SCENE_CAM_COUNT = int(os.environ.get("IMAG_SCENE_CAM_COUNT", "7"))
CAMS = range(1, IMAG_SCENE_CAM_COUNT + 1)
CANVAS_W, CANVAS_H, FPS = 1920, 1080, 60

# #791: the CANONICAL 17-scene operator layout (captured live off the incumbent .182 / replacement
# .187 -- byte-identical scene_order on both, 2026-07-27/28) -- the scenes seed() above OWNS
# ("Cam N"/"MV Cam N") plus the hand-built ones NO automated seeder creates ("resolume imag" /
# "MW resolume imag" / the base "Scene"). Never hardcode the Cam-N span here either -- derive it
# from CAMS so IMAG_SCENE_CAM_COUNT growth keeps this list correct with no second edit.
CANONICAL_SCENE_ORDER = (
    ["Scene"]
    + [f"Cam {n}" for n in reversed(list(CAMS))]
    + ["resolume imag"]
    + [f"MV Cam {n}" for n in CAMS]
    + ["MW resolume imag"]
)

# #791: the CANONICAL NDI-source bindings -- the 7 (or IMAG_SCENE_CAM_COUNT) fleet camera inputs
# seed() itself creates, PLUS the 3 Resolume/overlay inputs that live ONLY in the canonical scene
# collection JSON (scripts/imag-obs-scenes-canonical.json, installed by setup-imag.sh) -- no
# automated seeder creates those three, so verify_parity() below is what actually proves they
# exist and are bound to the right NDI sender name after a fresh provision.
CANONICAL_NDI_SOURCES = {
    **{f"NDI CAM{n}": f"CAM{n} (usb)" for n in CAMS},
    "MW imag resolume": "RESOLUME-SNV (Arena - To imag obs)",
    "NDI resolume imag": "RESOLUME-SNV (Arena - To imag obs)",
    "imag overlay": "RESOLUME-SNV (Arena - imag overlay)",
}


class Obs:
    def __init__(self, host: str, port: int, password: str | None):
        self.ws = create_connection(f"ws://{host}:{port}", timeout=10)
        hello = json.loads(self.ws.recv())["d"]
        ident = {"op": 1, "d": {"rpcVersion": 1}}
        if "authentication" in hello:
            if not password:
                sys.exit("FAIL: OBS WS requires auth but no --password given")
            auth = hello["authentication"]
            secret = base64.b64encode(
                hashlib.sha256((password + auth["salt"]).encode()).digest()
            ).decode()
            ident["d"]["authentication"] = base64.b64encode(
                hashlib.sha256((secret + auth["challenge"]).encode()).digest()
            ).decode()
        self.ws.send(json.dumps(ident))
        json.loads(self.ws.recv())
        self._rid = 0

    def req(self, req_type: str, data: dict | None = None, ignore_err: bool = False):
        self._rid += 1
        rid = str(self._rid)
        self.ws.send(json.dumps({"op": 6, "d": {
            "requestType": req_type, "requestId": rid, "requestData": data or {}}}))
        while True:
            msg = json.loads(self.ws.recv())
            if msg["op"] == 7 and msg["d"]["requestId"] == rid:
                st = msg["d"]["requestStatus"]
                if not st["result"] and not ignore_err:
                    sys.exit(f"FAIL: {req_type} -> {st.get('code')} {st.get('comment', '')}")
                return msg["d"].get("responseData", {})


# #847: RecEncoder hardware selection -- imag-nb recording never starts on a box with no
# discrete NVIDIA GPU.
#
# #502 hardcoded ("AdvOut", "RecEncoder", "obs_nvenc_h264_tex") against the INCUMBENT box's RTX
# 5050. The replacement notebook (10.77.9.187, #816) is Intel iGPU only -- NVENC never
# initializes ("Encoder ID 'obs_nvenc_h264_tex' not found" in the OBS log), so the recording
# output object is never created and every StartRecord silently produces 0 bytes (the exact [5/8]
# liveness-check failure this ticket fixes; same class as #709/#845).
#
# The obvious-looking fallback (QSV, obs_qsv11_v2 -- listed as loaded in the OBS log) was
# LIVE-TESTED on 10.77.9.187, not assumed (the #841 TearFree lesson: never ship a ported-by-
# analogy setting unverified). Three rounds, in order:
#   1. bare QSV -> "Failed to initialize MFX ... (MFX_ERR_NOT_FOUND)" -- `newlevel` was not in
#      the `render` group (/dev/dri/renderD128 is root:render).
#   2. render group fixed -> SAME error -- the oneVPL GPU runtime package (libmfx-gen1.2, the
#      actual hardware backend; only the dispatcher libvpl2 was installed) was missing too.
#   3. both gaps fixed -> StartRecord STILL never actually starts: "[qsv encoder: 'msdk_impl']
#      Unsupported configurations, parameters, or features (MFX_ERR_UNSUPPORTED)" at
#      MFXVideoENCODE::Init() (surf: Texture IOPattern) -- a genuine libmfx Texture/VAAPI-interop
#      incompatibility in OBS's Linux QSV plugin on this build, not a missing dependency.
# `obs_x264` (software), tested the SAME way, WORKS: StartRecord -> outputActive=True, bytes
# growing (1.0MB@3s/3.0MB@7s), a real playable 5.2MB .mkv from StopRecord. mpstat on the box's
# isolated cores (2-7, /etc/imag-isolated-cpus.conf) during that live recording showed ample
# headroom (cores 3/4/5/7 100% idle, core 6 ~10%; core 2's ~93% is the PRE-EXISTING #484
# SCHED_FIFO render-tick thread, unrelated to the encode). Full trail: the #847 issue's design
# comment. So: dGPU present -> NVENC (byte-for-byte unchanged), no dGPU -> x264 -- NEVER qsv,
# which is confirmed unreliable here (a follow-up issue tracks the QSV MFX_ERR_UNSUPPORTED
# finding for whoever wants to revisit it later; this ticket does not block on it, per its own
# "x264 is the safe fallback if QSV proves unreliable" guidance).


def select_rec_encoder(has_discrete_nvidia: bool) -> str:
    """Pure decision -- the OBS RecEncoder id to use for THIS box's hardware. See the comment
    block above for the live investigation that ruled out QSV as the no-dGPU fallback."""
    return "obs_nvenc_h264_tex" if has_discrete_nvidia else "obs_x264"


# Mirrors scripts/setup-imag.sh's `imag_has_discrete_nvidia` bash function EXACTLY (the SAME
# regex, same case-insensitivity, matched per-line like `grep`) -- that function cannot be
# imported from Python, so the regex is mirrored here rather than a second, differently-behaved
# detector invented (the #845 lesson). tests/python/test_imag_scenes_rec_encoder_847.py's
# test_has_discrete_nvidia_from_lspci_agrees_with_the_bash_detector runs BOTH against the same
# fixture text so a future drift in either regex is caught.
_NVIDIA_DISCRETE_RE = re.compile(
    r"(vga compatible controller|3d controller|display controller).*nvidia", re.IGNORECASE
)


def has_discrete_nvidia_from_lspci(lspci_output: str) -> bool:
    """Pure parser -- True iff LSPCI_OUTPUT (the text of `lspci -nn`) names a discrete NVIDIA
    display-class device on any line."""
    return any(_NVIDIA_DISCRETE_RE.search(line) for line in lspci_output.splitlines())


def _is_local_host(host: str) -> bool:
    """True when HOST resolves to this same machine -- the --bootstrap self-heal path
    (imag-obs-start.sh / imag-obs-watchdog.py) always passes literally 127.0.0.1."""
    return host in ("127.0.0.1", "localhost", "::1")


def _lspci_query_local() -> str:
    """Run `lspci -nn` as a LOCAL subprocess -- valid only when this process runs ON the box
    being configured (the loopback --host case). A missing lspci fails LOUD by name (#833 class)
    -- never silently read as "no discrete GPU"."""
    if shutil.which("lspci") is None:
        sys.exit(
            "FAIL: lspci not found on PATH -- cannot determine whether a discrete NVIDIA GPU is "
            "present (apt-get install -y pciutils)"
        )
    result = subprocess.run(["lspci", "-nn"], capture_output=True, text=True, timeout=10, check=False)
    return result.stdout


def _lspci_query_remote(host: str) -> str:
    """SSH to HOST and run `lspci -nn` remotely -- for the dev1-invoked case where imag_scenes.py
    runs on a DIFFERENT machine than the box being configured (verify-imag.sh, the manual
    post-provision step). Mirrors every other script's IMAG_USER/IMAG_PW sshpass convention
    (default newlevel/newlevel). A missing lspci (or sshpass/ssh) on the REMOTE box fails LOUD by
    name, never silently "no dGPU" (#833 class)."""
    user = os.environ.get("IMAG_USER", "newlevel")
    pw = os.environ.get("IMAG_PW", "newlevel")
    ssh_base = [
        "sshpass", "-p", pw, "ssh",
        "-o", "StrictHostKeyChecking=no", "-o", "ConnectTimeout=8",
        f"{user}@{host}",
    ]
    probe = subprocess.run(
        ssh_base + ["command -v lspci >/dev/null 2>&1 && echo LSPCI_OK || echo LSPCI_MISSING"],
        capture_output=True, text=True, timeout=15, check=False,
    )
    if "LSPCI_OK" not in probe.stdout:
        sys.exit(
            f"FAIL: lspci not found on {host} (or unreachable) -- cannot determine whether a "
            "discrete NVIDIA GPU is present (apt-get install -y pciutils)"
        )
    result = subprocess.run(ssh_base + ["lspci -nn"], capture_output=True, text=True, timeout=15, check=False)
    return result.stdout


def detect_has_discrete_nvidia(host: str) -> bool:
    """Detect whether HOST (the box being configured) has a discrete NVIDIA GPU -- local lspci
    when HOST is this same machine, remote SSH otherwise. Never guessed."""
    out = _lspci_query_local() if _is_local_host(host) else _lspci_query_remote(host)
    return has_discrete_nvidia_from_lspci(out)


def seed_profile(obs: Obs, has_discrete_nvidia: bool) -> None:
    """#502: put imag on a named ADVANCED profile with a native-1080p60 recording encoder, instead
    of the naive default Simple profile (x264 @ 6 Mbps 'Stream' quality, which softens the E2E
    QR/burns). imag records its OWN OBS-program output for the topology-v2 zero-loss verdict
    (recording-verdict-on-imag.sh extracts per-box partials from imag-REC.mkv), so a clean
    native-resolution recording matters (the #225 lesson: never let the recording rescale/soften).
    Applied over WebSocket (CreateProfile/SetCurrentProfile/SetProfileParameter) — verified to
    persist on the box. RecEncoder is now HARDWARE-SELECTED (#847, see select_rec_encoder above):
    NVENC h264 on a box with a discrete NVIDIA GPU (obs_nvenc_h264_tex, unchanged from #502),
    x264 (software, live-proven to work) on an Intel-iGPU-only box. RecRescale=false keeps it
    native 1080p either way."""
    rec_encoder = select_rec_encoder(has_discrete_nvidia)
    obs.req("CreateProfile", {"profileName": "imag-60fps"}, ignore_err=True)
    obs.req("SetCurrentProfile", {"profileName": "imag-60fps"}, ignore_err=True)
    for cat, name, val in (
        ("Output", "Mode", "Advanced"),
        ("AdvOut", "RecType", "Standard"),
        ("AdvOut", "RecEncoder", rec_encoder),
        ("AdvOut", "RecRescale", "false"),
        ("AdvOut", "RecFormat2", "mkv"),
        ("AdvOut", "RecFilePath", "/home/newlevel"),
    ):
        obs.req("SetProfileParameter", {
            "parameterCategory": cat, "parameterName": name, "parameterValue": val,
        }, ignore_err=True)
    prof = obs.req("GetProfileList").get("currentProfileName")
    mode = obs.req("GetProfileParameter", {
        "parameterCategory": "Output", "parameterName": "Mode"}).get("parameterValue")
    print(f"profile: {prof} (Mode={mode}, rec={rec_encoder} native-1080p mkv)")


def seed(obs: Obs) -> None:
    obs.req("SetVideoSettings", {
        "baseWidth": CANVAS_W, "baseHeight": CANVAS_H,
        "outputWidth": CANVAS_W, "outputHeight": CANVAS_H,
        "fpsNumerator": FPS, "fpsDenominator": 1,
    }, ignore_err=True)  # fails only while an output is active — report at verify

    for n in CAMS:
        scene, inp, ndi_name = f"Cam {n}", f"NDI CAM{n}", f"CAM{n} (usb)"
        # #783: detect pre-existence BEFORE create — an EXISTING item's transform belongs to
        # the OPERATOR (hand-tuned LED-wall crop/scale) and must NEVER be overwritten by a
        # boot/relaunch seed (live incident 2026-07-15: a hard reboot's autostart seed reset
        # the user's transforms to fullscreen). Only a freshly-created item gets the default.
        pre = obs.req("GetSceneItemId", {"sceneName": scene, "sourceName": inp},
                      ignore_err=True)
        item_existed = pre.get("sceneItemId") is not None
        obs.req("CreateScene", {"sceneName": scene}, ignore_err=True)
        obs.req("CreateInput", {
            "sceneName": scene, "inputName": inp, "inputKind": "ndi_source",
            "inputSettings": {"ndi_source_name": ndi_name, "latency": 1},  # 1 = Low latency
        }, ignore_err=True)
        # #785: source-binding/mute "self-healing" runs ONLY on --bootstrap (boot/recovery
        # path) or on a just-created input — a plain reseed must NEVER overwrite whatever
        # the OPERATOR set on an existing input (the "skripty mi kazia nastavenia" class).
        if BOOTSTRAP or not item_existed:
            obs.req("SetInputSettings", {
                "inputName": inp,
                "inputSettings": {"ndi_source_name": ndi_name, "latency": 1},
            }, ignore_err=True)
            obs.req("SetInputMute", {"inputName": inp, "inputMuted": True}, ignore_err=True)
        item = obs.req("GetSceneItemId", {"sceneName": scene, "sourceName": inp},
                       ignore_err=True)
        if not item_existed and item.get("sceneItemId") is not None:
            obs.req("SetSceneItemTransform", {
                "sceneName": scene, "sceneItemId": item["sceneItemId"],
                "sceneItemTransform": {
                    "boundsType": "OBS_BOUNDS_SCALE_INNER",
                    "boundsAlignment": 0,
                    "boundsWidth": CANVAS_W, "boundsHeight": CANVAS_H,
                    "positionX": 0, "positionY": 0,
                },
            }, ignore_err=True)

    # #501→SAME-SOURCE pivot (2026-07-15, user-driven): the "MV Cam N" cells now nest the SAME
    # full-bw "NDI CAMx" main inputs the program uses — identical frames, identical genlock
    # timing (3ms), zero proxy lag. The old low-bandwidth "MV CAMx" twin receivers are GONE:
    # their reason to exist (7x fullHD decode would collapse the notebook render) died with the
    # #767 keep-alive build (all main receivers decode continuously anyway); live-measured after
    # the switch: 60fps / 2.3ms render / 0 skips / CPU 3-12% — better than with the twins.
    for n in CAMS:
        scene, inp = f"MV Cam {n}", f"NDI CAM{n}"
        obs.req("CreateScene", {"sceneName": scene}, ignore_err=True)
        pre = obs.req("GetSceneItemId", {"sceneName": scene, "sourceName": inp},
                      ignore_err=True)
        item_existed = pre.get("sceneItemId") is not None
        if not item_existed:
            obs.req("CreateSceneItem", {"sceneName": scene, "sourceName": inp},
                    ignore_err=True)
        item = obs.req("GetSceneItemId", {"sceneName": scene, "sourceName": inp},
                       ignore_err=True)
        if not item_existed and item.get("sceneItemId") is not None:
            obs.req("SetSceneItemTransform", {
                "sceneName": scene, "sceneItemId": item["sceneItemId"],
                "sceneItemTransform": {
                    "boundsType": "OBS_BOUNDS_SCALE_INNER",
                    "boundsAlignment": 0,
                    "boundsWidth": CANVAS_W, "boundsHeight": CANVAS_H,
                    "positionX": 0, "positionY": 0,
                },
            }, ignore_err=True)

    # #785: forcing program/Studio is a BOOTSTRAP action (fresh OBS after boot/recovery).
    # A reseed on a RUNNING production OBS must never yank the operator's program scene.
    if BOOTSTRAP:
        obs.req("SetStudioModeEnabled", {"studioModeEnabled": True}, ignore_err=True)
        # #785: restore the program scene that was LIVE at the last graceful stop
        # (imag-obs-stop.sh writes ~/.config/imag-last-program before SIGTERM) instead of
        # always parking on "Cam 1" — the operator's cut must survive an OBS restart.
        # Unknown/stale scene name -> ignore_err leaves the collection default; missing
        # state file -> the old "Cam 1" fallback.
        program = "Cam 1"
        state_path = os.path.expanduser("~/.config/imag-last-program")
        try:
            with open(state_path) as fh:
                saved = fh.read().strip()
            if saved:
                program = saved
        except OSError as exc:
            print(f"seed: no last-program state ({state_path}: {exc}) — fallback '{program}'")
        obs.req("SetCurrentProgramScene", {"sceneName": program}, ignore_err=True)

    v = obs.req("GetVideoSettings")
    ok = (v["fpsNumerator"], v["baseWidth"], v["baseHeight"]) == (FPS, CANVAS_W, CANVAS_H)
    scenes = [s["sceneName"] for s in obs.req("GetSceneList")["scenes"]]
    missing = [f"Cam {n}" for n in CAMS if f"Cam {n}" not in scenes]
    mv_missing = [f"MV Cam {n}" for n in CAMS if f"MV Cam {n}" not in scenes]

    # #501: built-in-multiview membership — the SEED-OWNED scenes only: "MV Cam N" shown,
    # "Cam N" mains hidden (Stream Deck cuts the real scenes to program while the multiview
    # renders the MV set). Mirrors OBSBasic_Scenes.cpp's "ShowInMultiview" context-menu action
    # (same `show_in_multiview` key on obs_source_get_private_settings), applied over
    # SetSourcePrivateSettings.
    #
    # #785 OPERATOR-WINS — HARD RULE: a scene the seed does NOT own (anything outside
    # "Cam N"/"MV Cam N" — e.g. the operator's "MW resolume imag") is NEVER touched. The old
    # blanket `for name in scenes: show_in_multiview = name.startswith("MV Cam ")` actively
    # UN-TICKED the operator's own multiview choices on EVERY seed run (live incident chain,
    # 2026-07-16: the user re-ticked "MW resolume imag" and each seed wiped it again — this,
    # not lost unsaved UI state, was the recurring cause). Enforcement of even the OWNED
    # scenes' membership runs only on --bootstrap (fresh OBS); a bare seed touches nothing.
    if BOOTSTRAP:
        for name in scenes:
            owned = name.startswith("MV Cam ") or (name.startswith("Cam ") and name[4:].isdigit())
            if not owned:
                continue
            obs.req("SetSourcePrivateSettings", {
                "sourceName": name,
                "sourceSettings": {"show_in_multiview": name.startswith("MV Cam ")},
            }, ignore_err=True)

    print(f"video: {v['baseWidth']}x{v['baseHeight']}@{v['fpsNumerator']}"
          f"/{v['fpsDenominator']} {'OK' if ok else 'MISMATCH (output active? retry idle)'}")
    cam_count = len(list(CAMS))
    print(f"scenes: {len([s for s in scenes if s.startswith('Cam ')])}/{cam_count}"
          + (f" MISSING {missing}" if missing else " OK"))
    print(f"MV scenes: {len([s for s in scenes if s.startswith('MV Cam ')])}/{cam_count} (multiview, low-bw)"
          + (f" MISSING {mv_missing}" if mv_missing else " OK"))
    if not ok or missing or mv_missing:
        sys.exit(1)


def projector(obs: Obs) -> None:
    mons = obs.req("GetMonitorList")["monitors"]
    # Robust monitor selection across BOTH known imag-nb GPU generations (#522/#488): the older
    # Intel iGPU enumerated the built-in panel as "eDP-1"; this dGPU (RTX 5050 Laptop, PRIME
    # nvidia-primary, #500) enumerates it as "DP-0(0)" instead — an "eDP not in name" filter is
    # therefore AMBIGUOUS here (neither "DP-0(0)" nor "HDMI-0(1)" contains "eDP") and can wrongly
    # open PROGRAM on the panel instead of the projector (root cause of #522/#488). The one name
    # that stays STABLE across both generations is the connected monitor's own connector TYPE:
    # the external projector is always HDMI-*, the panel never is. Select on "HDMI"
    # presence/absence instead of "eDP" absence.
    hdmi = [m for m in mons if "HDMI" in m.get("monitorName", "")]
    if not hdmi:
        sys.exit("FAIL: no HDMI projector monitor detected — connect the HDMI monitor first "
                 f"(monitors: {[m.get('monitorName') for m in mons]})")
    obs.req("OpenVideoMixProjector", {
        "videoMixType": "OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM",
        "monitorIndex": hdmi[0]["monitorIndex"],
    })
    print(f"PROGRAM projector -> monitor {hdmi[0]['monitorIndex']} "
          f"({hdmi[0].get('monitorName')}) [HDMI]")

    # #507/#522: the built-in MULTIVIEW projector belongs on the panel (same monitor that shows
    # the OBS UI) so the cutter can see it without the HDMI projector output ever showing UI
    # chrome. Not fatal if no panel is found (e.g. a headless/remote debug session) — warn only.
    panel = [m for m in mons if "HDMI" not in m.get("monitorName", "")]
    if panel:
        obs.req("OpenVideoMixProjector", {
            "videoMixType": "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW",
            "monitorIndex": panel[0]["monitorIndex"],
        })
        print(f"MULTIVIEW projector -> monitor {panel[0]['monitorIndex']} "
              f"({panel[0].get('monitorName')}) [panel]")
    else:
        print("WARN: no panel monitor detected for the MULTIVIEW projector "
              f"(monitors: {[m.get('monitorName') for m in mons]})")


# #791: PURE comparison helpers (no OBS/network) -- unit-tested directly (tests/python), mirrors
# this repo's own "extract the pure decision, keep the WS glue thin" convention (src/reannounce.rs,
# src/colour_scale.rs). Kept separate from verify_parity() below so a fresh notebook's parity can
# be proven offline against a captured GetSceneList/GetInputSettings fixture, no rig required.

def scene_order_mismatch(actual_order: list, expected_order: list = None) -> str:
    """Returns "" iff actual_order == expected_order (CANONICAL_SCENE_ORDER by default) -- else a
    short human-readable description of what's wrong (missing/unexpected scenes, or a pure
    ordering drift when the SET matches but the sequence doesn't)."""
    expected = CANONICAL_SCENE_ORDER if expected_order is None else expected_order
    if actual_order == expected:
        return ""
    missing = [s for s in expected if s not in actual_order]
    extra = [s for s in actual_order if s not in expected]
    if missing or extra:
        parts = []
        if missing:
            parts.append(f"missing {missing}")
        if extra:
            parts.append(f"unexpected {extra}")
        return "; ".join(parts)
    return f"wrong ORDER -- got {actual_order}, want {expected}"


def ndi_source_mismatches(actual: dict, expected: dict = None) -> list:
    """actual/expected are {inputName: ndi_source_name}. Returns a list of human-readable problem
    strings (empty list = every expected binding present and correct). expected defaults to
    CANONICAL_NDI_SOURCES."""
    exp = CANONICAL_NDI_SOURCES if expected is None else expected
    problems = []
    for name, want in exp.items():
        if name not in actual:
            problems.append(f"MISSING {name!r}")
        elif actual[name] != want:
            problems.append(f"MISMATCH {name!r} -> got {actual[name]!r} want {want!r}")
    return problems


def verify_parity(obs: Obs) -> None:
    """#791: prove OPERATOR parity, not just system parity -- the FULL canonical 17-scene ORDER
    (not just "Cam N"/"MV Cam N" presence, which seed()'s own report already covers) and the 10
    canonical NDI-source bindings (7 fleet cams + the 3 Resolume/overlay inputs no automated
    seeder creates -- those live only in the canonical scene collection JSON installed by
    setup-imag.sh). Read-only: never seeds, creates, or touches anything.

    GetSceneList's own array order is the REVERSE of the scene collection JSON's scene_order
    field (live-verified 2026-07-28 against .187: WS index 0 = "MW resolume imag", last index =
    "Scene", while the on-disk JSON's scene_order lists "Scene" first) -- reverse it back before
    comparing against CANONICAL_SCENE_ORDER, which reads top-to-bottom the same way the JSON (and
    the ticket's own human-readable table) does.
    """
    ws_scenes = [s["sceneName"] for s in obs.req("GetSceneList")["scenes"]]
    actual_order = list(reversed(ws_scenes))
    order_problem = scene_order_mismatch(actual_order)
    print("scene order: " + (f"MISMATCH -- {order_problem}" if order_problem else "OK"))

    actual_ndi = {}
    for inp in obs.req("GetInputList").get("inputs", []):
        if inp.get("inputKind") != "ndi_source":
            continue
        name = inp["inputName"]
        settings = obs.req("GetInputSettings", {"inputName": name}, ignore_err=True)
        actual_ndi[name] = settings.get("inputSettings", {}).get("ndi_source_name")
    ndi_problems = ndi_source_mismatches(actual_ndi)
    print("ndi sources: " + ("; ".join(ndi_problems) if ndi_problems else "OK"))

    if order_problem or ndi_problems:
        sys.exit(1)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", required=True)
    ap.add_argument("--port", type=int, default=4455)
    ap.add_argument("--password", default=None)
    ap.add_argument("--bootstrap", action="store_true",
                    help="#785: boot/recovery invocation — enforce bindings/mutes/program/Studio "
                         "(a bare run only creates what is missing; operator state always wins)")
    ap.add_argument("--projector", action="store_true",
                    help="open the PROGRAM projector on the HDMI monitor AND the built-in "
                         "MULTIVIEW projector on the panel")
    ap.add_argument("--verify-parity", action="store_true",
                    help="#791: read-only check that the full canonical scene ORDER + NDI-source "
                         "bindings match (never seeds/creates anything)")
    args = ap.parse_args()
    obs = Obs(args.host, args.port, args.password)
    if args.projector:
        projector(obs)
    elif args.verify_parity:
        verify_parity(obs)
    else:
        # #847: detect once per run -- never guessed, see detect_has_discrete_nvidia above.
        has_dgpu = detect_has_discrete_nvidia(args.host)
        seed_profile(obs, has_dgpu)  # #502/#847: Advanced profile BEFORE the video/scene seed
        seed(obs)


if __name__ == "__main__":
    main()
