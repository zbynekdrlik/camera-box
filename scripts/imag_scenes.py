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
import shlex
import shutil
import subprocess
import sys
import time

# #785: --bootstrap = boot/recovery invocation (autostart + watchdog) — ONLY that path may
# enforce program scene / studio / input bindings; a bare run only creates what is missing.
BOOTSTRAP = "--bootstrap" in sys.argv

from websocket import create_connection

# #1143: the pure record-encoder logic (decision / VAAPI CQP settings / OBS-log parsers) lives in a
# sibling module so it stays Tier-0 pytest-testable on its own. imag_scenes.py runs either on the box
# (openbox autostart) or from dev1 with --host; both add THIS file's dir to sys.path[0] when run as a
# script, but the #847/#1143 importlib tests load this file by path, so add the dir explicitly.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import imag_record_encoder  # noqa: E402  (sibling module; needs the sys.path insert just above)

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


# ---------------------------------------------------------------------------
# issue 1218: active-set-aware NDI idle policy.
#
# ROOT CAUSE (measured live, dizajn-komentár): imag-nb thermal-throttles because it decodes camera
# NDI feeds OUTSIDE the active set for nothing -- an inactive camera's `NDI CAM{n}` receiver runs a
# full 1080p60 decode even though CAMERA_ACTIVE_SET retired it. seed() armed the baseline name for
# ALL of CAMS unconditionally, so an idled state never survived (OBS restart / --bootstrap / any
# reseed re-armed it). The fix: an active-set-aware policy so an INACTIVE camera's receiver is idled
# (ndi_source_name "" + genlock_fifo off -> DistroAV tears the receiver down, no decode) while an
# ACTIVE camera keeps its baseline name -- routed through ONE policy point every enforcement vector
# uses. overlay:True on every write preserves the per-source genlock_latency_ms_src 3ms pin.
#
# The active set reaches this module two ways: an explicit --active-cams flag (a dev1 caller sourcing
# scripts/camera-set.sh -- that pass ALSO writes a fresh copy of the one-line state file to the box),
# or, on the on-box --bootstrap self-heal, the provisioned state file (self-correcting staleness).
# No knowledge at all (no flag, no file) -> baseline-heal (the pre-1218 behavior), except a
# DELIBERATE idle is preserved via the #1158 discriminator below.

ACTIVE_CAMS_STATE_FILE = os.path.expanduser("~/.config/camera-box/imag-active-cams")


def parse_active_cams(text):
    """Pure (Tier-0): parse a CAMERA_ACTIVE_SET string ("cam1 cam2 cam6") into a set of camera
    NUMBERS ({1, 2, 6}). Whitespace/comma separated, case-insensitive; malformed tokens ignored.
    Returns None for None or a blank string with no valid token == "no set knowledge" (the caller
    then falls back to baseline-heal, the pre-1218 behavior)."""
    if text is None:
        return None
    nums = set()
    for tok in re.split(r"[\s,]+", text.strip()):
        m = re.fullmatch(r"cam(\d+)", tok.strip(), re.IGNORECASE)
        if m:
            nums.add(int(m.group(1)))
    return nums if nums else None


def format_active_cams(active_cams):
    """Pure: render an active-cams set back to the canonical "cam1 cam2 cam6" one-line form (sorted).
    None/empty -> ""."""
    if not active_cams:
        return ""
    return " ".join("cam%d" % n for n in sorted(active_cams))


def desired_ndi_state(n, active_cams):
    """Pure (Tier-0, no WS): the SetInputSettings inputSettings payload (always applied with
    overlay:True) for the imag `NDI CAM{n}` input under the active-set policy.
      - n in active_cams  -> {"ndi_source_name": "CAM{n} (usb)", "genlock_fifo": True}
      - n not in active   -> {"ndi_source_name": "", "genlock_fifo": False}  (idle receiver)
    Byte-for-byte the obs_phase2._idle_restore_settings(name) / ("") payload (parity-tested), so ONLY
    those two keys change and the per-source genlock_latency_ms_src 3ms pin (overlay:True) is
    preserved. active_cams MUST be a concrete set of ints (never None); the no-knowledge case is
    handled by ndi_policy_action, not here."""
    if n in active_cams:
        return {"ndi_source_name": "CAM%d (usb)" % n, "genlock_fifo": True}
    return {"ndi_source_name": "", "genlock_fifo": False}


def is_deliberate_idle(settings):
    """#1158 discriminator (pure): True iff `settings` (an input's inputSettings dict) is a
    DELIBERATE idle -- ndi_source_name == "" AND genlock_fifo is explicitly False. An accidental
    wedge (a mid-run reattach clear / a drifted saved scene that emptied the name) leaves genlock_fifo
    TRUE (or absent), so it reads False here -> a --bootstrap with NO set knowledge heals it to
    baseline, while a deliberate active-set idle is preserved. With set knowledge the set decides."""
    return settings.get("ndi_source_name", "") == "" and settings.get("genlock_fifo") is False


def ndi_policy_action(n, active_cams, current_settings=None):
    """Pure (Tier-0): the per-camera enforcement decision. Returns one of:
      ("reenforce", "CAM{n} (usb)")  -> (re)apply the baseline name + genlock on
      ("idle", "")                   -> apply the idle payload ("" + genlock off), read-back verify ""
      ("leave", None)                -> do nothing (preserve a deliberate idle when the set is unknown)
    active_cams: a set of active camera numbers, or None (no set knowledge). current_settings is
    consulted ONLY in the None branch (the #1158 wedge discriminator)."""
    name = "CAM%d (usb)" % n
    if active_cams is not None:
        return ("reenforce", name) if n in active_cams else ("idle", "")
    # no set knowledge -> heal a wedge to baseline, preserve a deliberate idle (#1158)
    if is_deliberate_idle(current_settings or {}):
        return ("leave", None)
    return ("reenforce", name)


def read_active_cams_state_file(path=None):
    """Read the one-line provisioned active-cams state file (default ACTIVE_CAMS_STATE_FILE, written
    by every dev1 --active-cams pass). Returns its stripped content, or None when the file is missing
    / empty / unreadable (self-correcting staleness: no file -> None -> baseline-heal)."""
    p = ACTIVE_CAMS_STATE_FILE if path is None else path
    try:
        with open(p) as fh:
            content = fh.read().strip()
        return content or None
    except OSError:
        return None


def resolve_active_cams(cli_value, state_path=None):
    """Resolve the active-cams set for this run, in priority order:
      1. the explicit --active-cams CLI value (a dev1 caller sourcing camera-set.sh),
      2. else the on-box state file (the --bootstrap self-heal reads its fresh copy),
      3. else None (no set knowledge -> baseline-heal; the pre-1218 behavior preserved).
    Returns a set of camera numbers, or None."""
    if cli_value is not None and cli_value.strip():
        return parse_active_cams(cli_value)
    return parse_active_cams(read_active_cams_state_file(state_path))


def write_active_cams_state_local(cli_value, path=None):
    """Write the one-line state file on THIS machine (the local-host case / provisioning). Overwrites
    idempotently; creates the parent dir. Raises OSError on a real write failure (caller warns)."""
    p = ACTIVE_CAMS_STATE_FILE if path is None else path
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "w") as fh:
        fh.write((cli_value or "").strip() + "\n")


def write_active_cams_state_remote(host, cli_value):
    """Push the one-line active-cams state file to a REMOTE box over ssh (the dev1 pass), so the box's
    next --bootstrap self-heal reads a fresh copy. Best-effort: a write failure only means the box
    keeps its previous state file (self-correcting on the next pass); it never aborts the seed."""
    line = (cli_value or "").strip()
    remote = ("mkdir -p ~/.config/camera-box && printf '%%s\\n' %s "
              "> ~/.config/camera-box/imag-active-cams") % shlex.quote(line)
    try:
        r = subprocess.run(_ssh_base(host) + [remote], capture_output=True, text=True,
                           timeout=15, check=False)
        if r.returncode != 0:
            print("WARN #1218: could not write imag-active-cams state to %s (rc=%d): %s"
                  % (host, r.returncode, r.stderr.strip()))
        else:
            print("#1218: wrote active-cams state '%s' to %s:~/.config/camera-box/imag-active-cams"
                  % (line, host))
    except Exception as e:  # noqa: BLE001 -- ssh transport failure is best-effort here
        print("WARN #1218: remote active-cams state write to %s failed: %s" % (host, e))


def _write_active_cams_state(host, cli_value):
    """Route the state-file write: local file on the box's own host, ssh push otherwise."""
    if _is_local_host(host):
        try:
            write_active_cams_state_local(cli_value)
            print("#1218: wrote active-cams state '%s' to %s"
                  % ((cli_value or "").strip(), ACTIVE_CAMS_STATE_FILE))
        except OSError as e:
            print("WARN #1218: local active-cams state write failed: %s" % e)
    else:
        write_active_cams_state_remote(host, cli_value)


def _obs_phase2_module():
    """Lazy import of the sibling obs_phase2.py (the SHARED #795-safe reenforce_ndi_name policy).
    Returns the module, or None when it is not importable on this host: the imag box installs
    imag_scenes.py (+ imag_record_encoder.py) and, since issue 1218, obs_phase2.py -- but an
    older box may not carry it yet, so the on-box --bootstrap enforce must DEGRADE to a direct set
    rather than crash the boot seed (the #1156 import-dependency class). Never imported at module
    load (imag-obs-start.sh's launch preflight only imports imag_scenes)."""
    try:
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        import obs_phase2  # noqa: E402
        return obs_phase2
    except Exception as e:  # noqa: BLE001 -- absence is expected on an older box; degrade, never crash
        print("#1218: obs_phase2 not importable (%s) -- active-name enforce uses a direct set "
              "(idle policy still applies; the discoverability gate is unavailable on this host)" % e)
        return None


def enforce_ndi_active_policy(obs, active_cams):
    """issue 1218: the ONE policy point for imag NDI-name enforcement -- every vector flows through
    it (the on-box --bootstrap seed and the dev1 --enforce-ndi-policy reenforce pass). For each
    camera n in CAMS, ndi_policy_action decides and this applies it:
      - "reenforce": (re)apply the baseline name. Discoverability-gated + read-back-verified via the
        SHARED obs_phase2.reenforce_ndi_name (#795-safe -- never sets a name absent from the finder).
        When obs_phase2 is unavailable (older box) OR the connection exposes no raw `ws` (a unit-test
        fake), it degrades to a direct overlay SetInputSettings of the baseline name + genlock on
        (the pre-1218 unconditional-arm behavior for an active cam).
      - "idle": apply the idle payload ("" + genlock_fifo off, overlay:True) and read-back verify the
        name is now "" -- a #1158 deliberate idle, never a silent failure to idle.
      - "leave": preserve a deliberate idle (no set knowledge).
    Returns {n: status} for the caller's log. Best-effort per camera (ignore_err); never raises on an
    OBS request error."""
    op = _obs_phase2_module()
    ws = getattr(obs, "ws", None)
    gated = op is not None and ws is not None
    need_current = active_cams is None
    result = {}
    for n in CAMS:
        inp = "NDI CAM%d" % n
        current = {}
        if need_current:
            current = (obs.req("GetInputSettings", {"inputName": inp}, ignore_err=True)
                       .get("inputSettings", {}) or {})
        action, name = ndi_policy_action(n, active_cams, current)
        if action == "leave":
            result[n] = "idle-preserved"
            continue
        if action == "reenforce":
            if gated:
                result[n] = "active:%s" % op.reenforce_ndi_name(ws, inp, name)
            else:
                obs.req("SetInputSettings", {
                    "inputName": inp,
                    "inputSettings": {"ndi_source_name": name, "genlock_fifo": True},
                    "overlay": True,
                }, ignore_err=True)
                result[n] = "active:set(ungated)"
            continue
        # action == "idle": tear the receiver down cold, then verify the name is really ""
        obs.req("SetInputSettings", {
            "inputName": inp,
            "inputSettings": {"ndi_source_name": "", "genlock_fifo": False},
            "overlay": True,
        }, ignore_err=True)
        back = (obs.req("GetInputSettings", {"inputName": inp}, ignore_err=True)
                .get("inputSettings", {}) or {}).get("ndi_source_name", "")
        result[n] = "idle:ok" if back == "" else "idle:VERIFY_FAILED(%r)" % back
    return result


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


def select_rec_encoder(has_discrete_nvidia: bool, available_encoders=None) -> str:
    """The OBS RecEncoder id for THIS box's hardware -- delegates to the Tier-0-tested pure decision
    in imag_record_encoder. #1143 CHANGED the no-dGPU choice from x264 to the Intel iGPU HARDWARE
    encoder ffmpeg_vaapi_tex (live-proven to record valid H.264 High 1080p60 while holding render at
    ~4ms/~0% lagged, eliminating the x264 observer effect #1130). x264 stays the graceful fallback
    when VAAPI is genuinely unavailable; QSV is NEVER chosen (#847 live-proved it broken here).
    ``available_encoders`` is None on the seed path (no OBS log to probe) -> trust the Intel bundle's
    ffmpeg_vaapi_tex; the E2E ensure-rec-encoder step passes the real advertised set."""
    return imag_record_encoder.choose_record_encoder(has_discrete_nvidia, available_encoders)


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
    ssh_base = _ssh_base(host)  # #769: shared sshpass-ssh base (same IMAG_USER/IMAG_PW convention)
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


def seed(obs: Obs, active_cams=None) -> None:
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
        # #1218: the ndi_source_name / genlock_fifo enforcement moved OUT of this loop to the
        # ONE active-set-aware policy point (enforce_ndi_active_policy, called after the loop on
        # --bootstrap) — so an inactive camera is idled instead of unconditionally re-armed. Here
        # we re-arm only the active-set-INDEPENDENT settings: DistroAV low-latency mode + mute
        # (overlay:True merges, leaving the name for the policy to own).
        if BOOTSTRAP or not item_existed:
            obs.req("SetInputSettings", {
                "inputName": inp,
                "inputSettings": {"latency": 1},
                "overlay": True,
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

    # #1218: active-set-aware NDI-name enforcement — the ONE policy point. On --bootstrap (autostart
    # + watchdog reseed) idle every INACTIVE camera's receiver (ndi_source_name "" + genlock_fifo off
    # so imag stops decoding it — the thermal-throttle root cause), (re)enforce every ACTIVE camera's
    # baseline name (discoverability-gated when obs_phase2 is available), and preserve a deliberate
    # idle when the active set is unknown (#1158 wedge discriminator). A bare (non-bootstrap) reseed
    # never enforces here (the #785 operator-wins discipline); the dev1 --enforce-ndi-policy mode is
    # the immediate reenforce path.
    if BOOTSTRAP:
        statuses = enforce_ndi_active_policy(obs, active_cams)
        print("ndi policy (active=%s): %s"
              % (format_active_cams(active_cams) or "unknown(baseline-heal)",
                 ", ".join("CAM%d=%s" % (n, s) for n, s in sorted(statuses.items()))))

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


# #769: projector count-first idempotence (windowed-stray dedup).
#
# obs-websocket's OpenVideoMixProjector ALWAYS opens a NEW window (the protocol has no "is a
# projector open" query), and OBS's own CloseExistingProjectors replace-loop closes only projectors
# whose internal GetMonitor()==the target monitor -- so a launch-restore stray (windowed, internal
# monitor=-1) is invisible to it and every blind seed stacks one more (live "3x Multiview, gate
# refuse", 2026-07-15). projector() below opens the projector, then closes the OLDER stray windows,
# keeping the NEWEST X window id (== the one it just opened on the correct monitor). Done in the
# SEEDER itself so the stack never forms on the LIVE box between gate runs -- the watchdog inherits
# it for free (imag-obs-watchdog.py calls this same seed). Mirrors the already-merged gate heal
# (scripts/lib/imag-projector-heal.sh); the pure decision is extracted here for an offline
# wmctrl-fixture unit test (the #791 pure-seam pattern).


def projector_window_ids(wmctrl_output, kind):
    """Ids (in file order) of every `Projector - <kind>` window in a `wmctrl -l` dump. `kind` is
    "Multiview" or "Program". A wmctrl line is `0xID  <desktop>  <host>  <title>`; we match on the
    title marker `Projector - <kind>` (the only place that string ever appears -- same substring the
    gate heal / count-check / verify-imag all key on) and take the leading 0x window id."""
    marker = "Projector - %s" % kind
    ids = []
    for line in (wmctrl_output or "").splitlines():
        if marker not in line:
            continue
        parts = line.split()
        tok = parts[0] if parts else ""
        if re.match(r"^0x[0-9a-fA-F]+$", tok):
            ids.append(tok)
    return ids


def projector_strays_to_close(wmctrl_output, kind):
    """The window ids to CLOSE so exactly one `Projector - <kind>` survives -- every id EXCEPT the
    newest (highest NUMERIC X id == the projector just opened on the correct monitor). Returns []
    when 0 or 1 windows exist (nothing to heal -> a repeated seed is a no-op, the idempotence
    acceptance criterion). The survivor is chosen numerically, not lexicographically, so a
    wmctrl/`sort` ordering of unequal-length ids can never pick the wrong one.

    MIRROR NOTE: the already-merged bash gate heal (scripts/lib/imag-projector-heal.sh) picks the
    survivor with `sort | tail -1` (LEXICOGRAPHIC) -- equivalent here because real wmctrl ids are
    fixed-width `0x`+8 hex digits (lexicographic == numeric), and this numeric form is strictly the
    more robust of the two. The two mirrored keep-newest copies must stay behaviorally aligned.
    keep-newest assumes no XID recycling of a freed lower id onto the fresh window (vanishingly
    unlikely per-client id space); the gate `[0/8]` 1+1 count check is the backstop if it ever bit."""
    ids = projector_window_ids(wmctrl_output, kind)
    if len(ids) <= 1:
        return []
    newest = max(ids, key=lambda x: int(x, 16))
    return [i for i in ids if i != newest]


def _wmctrl_list_local():
    """`wmctrl -l` from the LOCAL X display (the loopback --host boot/watchdog path, DISPLAY=:0).
    Returns None when wmctrl is absent -- the caller warns LOUD by name and skips dedup, NEVER
    silently reads a missing tool as "no windows" (imag-ssh-remote-tool-preflight rule). Never
    raises (runs under imag-obs-start.sh's set -euo pipefail)."""
    if shutil.which("wmctrl") is None:
        return None
    try:
        env = dict(os.environ)
        env.setdefault("DISPLAY", ":0")
        r = subprocess.run(["wmctrl", "-l"], capture_output=True, text=True,
                           timeout=10, check=False, env=env)
        if r.returncode != 0:
            # #833: a PRESENT-but-failing wmctrl (X unreachable, nonzero exit) must NOT be read as
            # "0 windows" -- return None so the caller warns + skips dedup, same as a missing tool.
            print("WARN #769: local wmctrl -l exited %d -- cannot enumerate projector windows"
                  % r.returncode)
            return None
        return r.stdout
    except Exception as e:
        print("WARN #769: local wmctrl -l failed: %s" % e)
        return None


def _wmctrl_close_local(win_id):
    """Close one X window by id on the local display. Best-effort, never raises (a failed close is
    warned, not fatal -- the gate [0/8] count check catches a stray that survives). Injects
    DISPLAY=:0 exactly like _wmctrl_list_local (symmetric with the remote close's `DISPLAY=:0`
    prefix) so a caller with no DISPLAY set never ENUMERATES strays yet silently fails to CLOSE
    them."""
    try:
        env = dict(os.environ)
        env.setdefault("DISPLAY", ":0")
        subprocess.run(["wmctrl", "-i", "-c", win_id], timeout=10, check=False, env=env)
    except Exception as e:
        print("WARN #769: local wmctrl -c %s failed: %s" % (win_id, e))


def _ssh_base(host):
    user = os.environ.get("IMAG_USER", "newlevel")
    pw = os.environ.get("IMAG_PW", "newlevel")
    return ["sshpass", "-p", pw, "ssh",
            "-o", "StrictHostKeyChecking=no", "-o", "ConnectTimeout=8",
            "%s@%s" % (user, host)]


def _wmctrl_list_remote(host):
    """`wmctrl -l` over ssh (the rare dev1-invoked remote --host case). Mirrors _lspci_query_remote:
    probe wmctrl exists first, fail loud by NAME (return None -> caller warns + skips) if it does
    not, never silently "no windows". Never raises."""
    ssh_base = _ssh_base(host)
    try:
        probe = subprocess.run(
            ssh_base + ["command -v wmctrl >/dev/null 2>&1 && echo WMCTRL_OK || echo WMCTRL_MISSING"],
            capture_output=True, text=True, timeout=15, check=False)
        if "WMCTRL_OK" not in probe.stdout:
            return None
        r = subprocess.run(ssh_base + ["DISPLAY=:0 wmctrl -l"],
                           capture_output=True, text=True, timeout=15, check=False)
        if r.returncode != 0:
            # #833: a nonzero rc (ssh drop OR a failing remote wmctrl) means the read is untrusted --
            # return None (warn + skip dedup), never treat empty output as "0 windows".
            print("WARN #769: remote wmctrl -l on %s exited %d -- cannot enumerate"
                  % (host, r.returncode))
            return None
        return r.stdout
    except Exception as e:
        print("WARN #769: remote wmctrl -l on %s failed: %s" % (host, e))
        return None


def _wmctrl_close_remote(host, win_id):
    """Close one X window by id over ssh. Best-effort, never raises."""
    try:
        subprocess.run(_ssh_base(host) + ["DISPLAY=:0 wmctrl -i -c %s" % win_id],
                       capture_output=True, text=True, timeout=15, check=False)
    except Exception as e:
        print("WARN #769: remote wmctrl -c %s on %s failed: %s" % (win_id, host, e))


def _heal_projector_strays(host, kinds):
    """#769 count-first dedup: AFTER opening, close any OLDER stray windows per kind (keep the newest
    == the one just opened on the correct monitor), so blind re-seeds/launch-restore can never stack.
    Local wmctrl when HOST is loopback (boot/watchdog), else sshpass ssh (dev1 manual). A missing
    wmctrl warns LOUD by name and SKIPS dedup (the projectors are already open; the gate [0/8] count
    check is the backstop) -- never aborts OBS start, never reads a missing tool as "no windows"."""
    local = _is_local_host(host)
    time.sleep(1)  # let the freshly-opened window register in the WM before enumerating
    out = _wmctrl_list_local() if local else _wmctrl_list_remote(host)
    if out is None:
        print("WARN #769: wmctrl not available (%s) -- cannot dedup projector windows this run; "
              "projectors are open, the gate [0/8] count check remains the backstop"
              % ("local" if local else host))
        return
    for kind in kinds:
        for win_id in projector_strays_to_close(out, kind):
            if local:
                _wmctrl_close_local(win_id)
            else:
                _wmctrl_close_remote(host, win_id)
            print("healed #769: closed stray %s projector %s (kept newest on the correct monitor)"
                  % (kind, win_id))


# issue 1152 M4: DRM-lease mode -- the vendored OBS DRM output (.claude/rules/obs-drm-output.md)
# leases the HDMI connector OUT of the X layout and page-flips the Program onto it directly, so
# in that mode there is NO HDMI monitor for an X Program projector and none is wanted. The config
# below is the module's own DEFAULT-OFF activation contract; this seeder consults the SAME file
# so every caller (unit boot, operator menu, watchdog relaunch, verify repopulate) inherits the
# tolerance from ONE place. NOTHING in these helpers may abort: they run on the supervised OBS
# start path, where a non-zero exit crash-loops a healthy OBS on the live projection (issue 866;
# the live 2026-08-26 M1 runbook gotcha).
DRM_OUTPUT_CONF = "~/.camera-box/drm-output.json"


def drm_output_lease_connector(config_text):
    """issue 1152 pure: the connector name IFF the drm-output config JSON arms the in-OBS
    DRM-lease output, else "". Mirrors the vendored C module's OWN contract exactly
    (obs-drm-output.c): full JSON parse (unparseable -> dormant), a boolean "enabled": true,
    AND a non-empty string "connector" (the C is dormant without one). Matching the C is the
    review-mandated single grammar: a config the C would ignore must NEVER arm the wrapper or
    the seeder (a divergent bash-grep reading once re-opened the crash-loop / dark-projector
    class this milestone kills). Empty / missing / malformed -> "" (dormant), never a raise."""
    if not config_text:
        return ""
    try:
        cfg = json.loads(config_text)
        if cfg.get("enabled") is not True:
            return ""
        connector = cfg.get("connector")
        return connector if isinstance(connector, str) and connector else ""
    except (ValueError, AttributeError):
        return ""


def drm_output_lease_enabled(config_text):
    """issue 1152 pure: True iff drm_output_lease_connector() arms -- ONE classifier, one
    grammar (the C module's), for every consumer."""
    return drm_output_lease_connector(config_text) != ""


def _drm_output_config_text(host):
    """issue 1152: read the BOX's own drm-output config -- a local open on the loopback
    boot/watchdog path, the SAME sshpass transport the wmctrl helpers use for a dev1-driven
    --host <ip> call (NEVER the calling machine's own file in that case). Any failure -> ""
    (the dormant default; the drm_output drift facet, not this seeder, owns loud enabled-state
    verdicts)."""
    if _is_local_host(host):
        try:
            with open(os.path.expanduser(DRM_OUTPUT_CONF)) as fh:
                return fh.read()
        except OSError:
            return ""
    try:
        r = subprocess.run(_ssh_base(host) + ["cat %s 2>/dev/null || true" % DRM_OUTPUT_CONF],
                           capture_output=True, text=True, timeout=15, check=False)
        return r.stdout if r.returncode == 0 else ""
    except Exception:
        return ""


def _drm_lease_close_program_strays(host):
    """issue 1152: in DRM-lease mode EVERY X "Projector - Program" window is a stray (OBS's
    launch-restore can recreate the saved one WINDOWED on the panel once the HDMI monitor left
    X) -- the Program lives on the DRM scanout, so close them ALL. Same wmctrl transport +
    missing-tool warn-skip discipline as _heal_projector_strays: warn LOUD by name, never raise,
    never read a missing tool as "no windows"."""
    local = _is_local_host(host)
    out = _wmctrl_list_local() if local else _wmctrl_list_remote(host)
    if out is None:
        print("WARN issue 1152: wmctrl not available (%s) -- cannot close restored X Program "
              "projector strays this run" % ("local" if local else host))
        return
    for win_id in projector_window_ids(out, "Program"):
        if local:
            _wmctrl_close_local(win_id)
        else:
            _wmctrl_close_remote(host, win_id)
        print("issue 1152 drm-lease mode: closed restored X Program projector %s (the Program "
              "is on the DRM scanout, not an X window)" % win_id)


def _projector_drm_lease_mode(obs, host):
    """issue 1152: the lease-mode projector seed -- open ONLY the panel Multiview (the Program
    page-flips on the DRM-leased connector; opening an X Program projector would put a duplicate
    window on the panel), close every restored X Program stray, and reconcile the Multiview via
    the #769 keep-newest dedup. Loud on every branch, aborts on none."""
    print("issue 1152 drm-lease mode ENABLED (%s): Program goes out via the DRM-leased HDMI "
          "scanout -- skipping the X Program projector, opening ONLY the panel Multiview "
          "projector" % DRM_OUTPUT_CONF)
    mons = obs.req("GetMonitorList")["monitors"]
    panel = [m for m in mons if "HDMI" not in m.get("monitorName", "")]
    opened_kinds = []
    if panel:
        obs.req("OpenVideoMixProjector", {
            "videoMixType": "OBS_WEBSOCKET_VIDEO_MIX_TYPE_MULTIVIEW",
            "monitorIndex": panel[0]["monitorIndex"],
        })
        print(f"MULTIVIEW projector -> monitor {panel[0]['monitorIndex']} "
              f"({panel[0].get('monitorName')}) [panel]")
        opened_kinds.append("Multiview")
    else:
        print("WARN: no panel monitor detected for the MULTIVIEW projector "
              f"(monitors: {[m.get('monitorName') for m in mons]})")
    _drm_lease_close_program_strays(host)
    if opened_kinds:
        _heal_projector_strays(host, opened_kinds)


def projector(obs: Obs, host: str) -> None:
    # issue 1152 M4: with the DRM output ENABLED the HDMI connector is leased out of the X
    # layout BY DESIGN -- the old hard fail-exit below would then crash-loop the supervised unit
    # (the live 2026-08-26 M1 runbook gotcha: restart counter climbing every ~13 s). Consult the
    # box's own config FIRST; dormant boxes fall through to the original behaviour unchanged,
    # including the genuinely-unplugged-HDMI loud failure.
    if drm_output_lease_enabled(_drm_output_config_text(host)):
        _projector_drm_lease_mode(obs, host)
        return
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
    opened_kinds = ["Program"]

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
        opened_kinds.append("Multiview")
    else:
        print("WARN: no panel monitor detected for the MULTIVIEW projector "
              f"(monitors: {[m.get('monitorName') for m in mons]})")

    # #769: count-first dedup -- after opening, close any OLDER stray windows so a launch-restore
    # stray + this seed can never accumulate (keep the newest = the one just opened). Only the kinds
    # actually opened above are reconciled; a missing panel/wmctrl never aborts (see the helper).
    _heal_projector_strays(host, opened_kinds)


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


def ndi_source_mismatches(actual: dict, expected: dict = None, active_cams=None) -> list:
    """actual/expected are {inputName: ndi_source_name}. Returns a list of human-readable problem
    strings (empty list = every expected binding present and correct). expected defaults to
    CANONICAL_NDI_SOURCES.

    #1218: when active_cams is given, an INACTIVE camera's `NDI CAM{n}` receiver is idled, so its
    EXPECTED binding is "" (not the baseline name) — a correctly-idled inactive camera is NOT a
    problem, while an inactive camera still bound to a name IS flagged (NOT IDLE). active_cams=None
    keeps the pre-1218 behavior (baseline expected for every camera)."""
    exp = CANONICAL_NDI_SOURCES if expected is None else expected
    problems = []
    for name, want in exp.items():
        if active_cams is not None:
            m = re.fullmatch(r"NDI CAM(\d+)", name)
            if m is not None and int(m.group(1)) not in active_cams:
                want = ""  # inactive camera: idled receiver -> expected binding is ""
        if name not in actual:
            problems.append(f"MISSING {name!r}")
        elif actual[name] != want:
            if want == "":
                problems.append(f"NOT IDLE {name!r} -> got {actual[name]!r} want '' (active-set idle)")
            else:
                problems.append(f"MISMATCH {name!r} -> got {actual[name]!r} want {want!r}")
    return problems


def active_set_idle_report(actual: dict, active_cams) -> list:
    """Pure: the `NDI CAM{n}` inputs CORRECTLY idled under the active set (n inactive AND the input's
    ndi_source_name is currently "") — for verify_parity's `idle(active-set)` print line. Empty when
    active_cams is None (no set knowledge)."""
    if active_cams is None:
        return []
    out = []
    for name in CANONICAL_NDI_SOURCES:
        m = re.fullmatch(r"NDI CAM(\d+)", name)
        if m is not None and int(m.group(1)) not in active_cams and actual.get(name) == "":
            out.append(name)
    return out


def verify_parity(obs: Obs, active_cams=None) -> None:
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
    ndi_problems = ndi_source_mismatches(actual_ndi, active_cams=active_cams)
    idled = active_set_idle_report(actual_ndi, active_cams)
    print("ndi sources: " + ("; ".join(ndi_problems) if ndi_problems else "OK")
          + (f" | idle(active-set): {idled}" if idled else ""))

    if order_problem or ndi_problems:
        sys.exit(1)


# #866 -- force the measurement burn OFF at OBS start so a saved genlock_burn=true can never
# survive a restart onto the live IMAG projection.
#
# The per-source measurement-burn bool (genlock_burn -- obs_burn_filter.py's BURN_SETTING, #257)
# is persisted INTO OBS's saved scene collection; turning it OFF is only ever a RUNTIME WebSocket
# change (the gate cleanup / obs_burn_filter.py remove -- never written to disk unless OBS exits
# cleanly). So an OBS crash/reboot/manual restart reloads the disk copy = true and RENDERS the QR
# burn onto the live projection (ticket's live evidence: Untitled.json NDI CAM1 burn=True surviving
# a segfault-restart). imag-obs-start.sh (the box's own start path -- boot autostart, operator
# "Spustit OBS", imag-obs.service Restart=on-failure) runs `imag_scenes.py --bootstrap` on every
# fresh instance, so clearing burns there closes the restart-resurrection window.
#
# imag_scenes.py is fetched STANDALONE to /usr/local/bin (setup-imag.sh) and cannot import the
# obs_burn_filter.py shared seam (not deployed on the box), so this reimplements the tiny
# enumerate-ndi + clear logic locally -- the SAME established imag precedent as imag_latency_enforce
# .py's own list_ndi_inputs/enforce loop (#757). The "route through ONE seam" rule (burn-target-
# enumeration.md) governs the SHELL consumers (rig-mode, recording-e2e) that CAN import the seam.
BURN_SETTING = "genlock_burn"  # obs_burn_filter.BURN_SETTING (#257) -- the per-source burn bool


def ndi_source_names(inputs: list) -> list:
    """Pure: inputName of every `ndi_source` input in a GetInputList `inputs` array, in order.

    The measurement burn only applies to ndi_source inputs, so those are exactly the inputs a burn
    can leak onto -- enumerate them from OBS reality, never a static/CAMS list (burn-target-
    enumeration rule). Skips malformed entries (no/empty name) and non-ndi kinds."""
    return [i.get("inputName") for i in (inputs or [])
            if i.get("inputKind") == "ndi_source" and i.get("inputName")]


def clear_measurement_burns(obs: Obs) -> None:
    """#866: force the measurement burn OFF on EVERY ndi_source input at OBS start.

    Enumerates from OBS reality (GetInputList), clears genlock_burn=false only on the inputs that
    currently have it ON (SetInputSettings overlay-merge -- never clobber the source's other
    settings) and read-back verifies each. A measurement burn is never legitimate operator state
    (unlike the #785 bindings/transforms this module protects), so forcing it OFF here never fights
    the operator. Called on the --bootstrap (fresh-instance) path only, so a plain reseed from dev1
    never nukes a burn a gate run deliberately set mid-measurement.

    This is a best-effort START-TIME sweep and NEVER aborts: imag-obs-start.sh runs it under
    `set -euo pipefail`, so a SystemExit / an uncaught WS exception (a #328 timeout raise, a closed
    socket) here would take OBS DOWN on the live projection box -- and, under systemd
    Restart=on-failure, LOOP it while the same transient keeps failing -- which is worse than a
    visible+logged burn. So every failure mode (enumeration failure, a mid-sweep WS error, OR a
    clear that will not land) is warned LOUD (captured to /tmp/imag-obs-start.log) and returns,
    leaving OBS up. The next gate run's [0/8] exhaustive sweep-off is the AUTHORITATIVE fail-closed
    backstop that refuses to certify a leak; this start-time pass just closes the common
    restart-resurrection window without ever being able to break OBS startup."""
    try:
        resp = obs.req("GetInputList", ignore_err=True)
        if not isinstance(resp, dict) or "inputs" not in resp:
            # A live imag OBS always has ndi inputs, so a missing `inputs` means "could not
            # enumerate", never "no burns" (#1011 fail-closed lesson) -- warn, never read as clean.
            print("burns: WARNING #866/#1011 -- GetInputList FAILED at start; could NOT "
                  "verify/clear measurement burns. The next gate's [0/8] sweep-off is the backstop.")
            return
        names = ndi_source_names(resp["inputs"])
        cleared, still_on = [], []
        for name in names:
            # cur is None both for a read hiccup AND for the (common) case of an input that never
            # carried a burn -- indistinguishable, so a None is correctly SKIPPED, never counted as
            # a leak (counting it would false-warn on every ordinary non-burn ndi input). Only a
            # read-back that is still True after the clear is a real still-on.
            cur = (obs.req("GetInputSettings", {"inputName": name}, ignore_err=True)
                   .get("inputSettings", {}).get(BURN_SETTING))
            if cur is True:
                obs.req("SetInputSettings", {
                    "inputName": name,
                    "inputSettings": {BURN_SETTING: False},
                    "overlay": True,  # merge -- never clobber the source's other (forced) settings
                }, ignore_err=True)
                rb = (obs.req("GetInputSettings", {"inputName": name}, ignore_err=True)
                      .get("inputSettings", {}).get(BURN_SETTING))
                (cleared if rb in (False, None) else still_on).append(name)
    except Exception as exc:  # noqa: BLE001 -- ANY WS error (a #328 timeout raise) must NOT crash
        # the OBS start path; mirrors obs_burn_filter._all_ndi_inputs' try/except -> fail-safe.
        print(f"burns: WARNING #866/#1011 -- OBS WS error during start-time burn clear ({exc!r}); "
              f"could NOT fully verify/clear measurement burns. [0/8] sweep-off is the backstop.")
        return
    if cleared:
        print(f"burns: forced OFF at start on {len(cleared)} ndi input(s) (#866): {cleared}")
    else:
        print(f"burns: none ON at start ({len(names)} ndi input(s) scanned) (#866)")
    if still_on:
        # A clear that did not land IS a real leak, but do NOT SystemExit (restart-loop hazard
        # above) -- warn LOUD and leave OBS up; the [0/8] sweep-off is the authoritative backstop.
        print(f"burns: WARNING #866/#1011 -- genlock_burn STILL ON after clear on {still_on} -- a "
              f"burn may be rendering on the live IMAG projection; investigate OBS WS. The next "
              f"gate run's [0/8] sweep-off is the authoritative backstop.")


# ======================= #1143 record-encoder ensure (make-it-live) =======================
# The IMPURE make-it-live orchestration for the imag OBS record encoder. The pure decision +
# settings + apply-plan live in imag_record_encoder (Tier-0 pytest-tested); this wires them to the
# box. A record-encoder value written over the WebSocket does NOT take effect on an already-running
# OBS -- OBS only rebuilds the Advanced-output encoder at (re)start / ResetOutputs (#847). So this
# applies the #847-proven ordering ONLY when the pure apply-plan says the disk config is not already
# the live HW target: WS SetProfileParameter(RecEncoder) FIRST (survives OBS's own shutdown save) ->
# systemctl --user stop imag-obs -> write recordEncoder.json while OBS is DOWN (a running OBS would
# clobber it on a clean-shutdown save) -> systemctl --user start imag-obs -> reconnect WS + verify.
# Called by recording-e2e.sh at pre-record, EARLY (before the #882 render-health warm-up window,
# which absorbs the post-restart NDI/shader settle). It is NEVER on the OBS start path (#866), so a
# genuinely-down OBS after the restart fails LOUD (the E2E can't run anyway); a config hiccup that
# leaves OBS UP is best-effort (warn + return -- the verdict's report-only lagged% still catches a
# stale x264 encoder and attributes the observer-effect confound). Restart via the USER unit only
# (#1015 -- never a direct imag-obs-start.sh call, which bypasses supervision).
_ENSURE_XDG = "export XDG_RUNTIME_DIR=/run/user/$(id -u); "


def _ssh_capture(host, remote_cmd, timeout=30):
    """Run REMOTE_CMD on HOST (local shell on loopback, ssh otherwise); return (rc, stdout). Never
    raises -- a failure is (nonzero, '') for the caller to fail-open on."""
    argv = (["bash", "-lc", remote_cmd] if _is_local_host(host)
            else _ssh_base(host) + [remote_cmd])
    try:
        r = subprocess.run(argv, capture_output=True, text=True, timeout=timeout, check=False)
        return r.returncode, r.stdout
    except Exception as e:  # noqa: BLE001 -- ssh/timeout hiccup must not crash the ensure step
        print(f"[ensure-rec-encoder] WARN: remote cmd failed ({e})")
        return 1, ""


def _obs_available_encoders(host):
    """The video encoder ids OBS advertised (newest OBS log 'Available Encoders' block). None on any
    read failure -> select_rec_encoder trusts the Intel bundle default (#1143)."""
    rc, out = _ssh_capture(
        host, "LOG=$(ls -t ~/.config/obs-studio/logs/*.txt 2>/dev/null | head -1); "
              "sed -n '1,220p' \"$LOG\" 2>/dev/null")
    if rc != 0 or not out:
        return None
    return imag_record_encoder.parse_available_encoders(out) or None


def _ws_connect(host, port, password):
    """Connect the OBS WebSocket with a CLEAN error on a down OBS instead of a raw traceback.
    ensure-rec-encoder is best-effort (recording-e2e.sh wraps the call and the [1/8] render-health
    preflight then catches a genuinely down OBS), so a clean nonzero exit here is the right shape."""
    try:
        return Obs(host, port, password)
    except Exception as e:  # noqa: BLE001 -- OBS WS unreachable; fail clean, never a traceback
        sys.exit(f"FAIL: imag OBS WebSocket unreachable at {host}:{port} ({e}) — cannot ensure the "
                 "record encoder (best-effort; the render-health preflight catches a genuinely down OBS)")


def _current_profile_dir(host, port, password):
    """Resolve the on-disk profile DIR for the CURRENT OBS profile. OBS strips non-alphanumeric
    chars from the display name for the dir ('imag-60fps' -> 'imag60fps'); verified against the live
    profiles/ listing, falling back to the single non-'Untitled' dir. Env override IMAG_PROFILE_DIR."""
    override = os.environ.get("IMAG_PROFILE_DIR")
    if override:
        return override
    obs = _ws_connect(host, port, password)
    try:
        name = obs.req("GetProfileList", ignore_err=True).get("currentProfileName", "")
    finally:
        obs.ws.close()
    cand = re.sub(r"[^A-Za-z0-9]", "", name)
    _, out = _ssh_capture(host, "ls ~/.config/obs-studio/basic/profiles/ 2>/dev/null")
    dirs = [d for d in out.split() if d]
    if cand and cand in dirs:
        return cand
    non_default = [d for d in dirs if d != "Untitled"]
    if len(non_default) == 1:
        return non_default[0]
    if cand:
        return cand
    sys.exit(f"FAIL: cannot resolve the imag OBS profile dir (name={name!r} dirs={dirs}) — "
             "set IMAG_PROFILE_DIR")


def _read_rec_encoder_config(host, profile_dir):
    """(current [AdvOut] RecEncoder or None, recordEncoder.json text or '') read off the box.

    basic.ini has TWO `RecEncoder=` keys — `[SimpleOutput]` (`x264`) and `[AdvOut]` (`obs_x264`).
    The advanced record output (Mode=Advanced here) reads the `[AdvOut]` one, so a plain
    `grep -m1 '^RecEncoder='` would wrongly return the SimpleOutput value and break idempotency
    (always reading `x264` -> always 'apply' -> a restart every run). Scope the read to `[AdvOut]`."""
    base = f"~/.config/obs-studio/basic/profiles/{profile_dir}"
    adv = ("awk '/^\\[AdvOut\\]/{a=1;next} /^\\[/{a=0} "
           "a && /^RecEncoder=/{sub(/^RecEncoder=/,\"\");print;exit}'")
    _, out = _ssh_capture(
        host, f"{adv} {base}/basic.ini 2>/dev/null; "
              f"echo '---SEP---'; cat {base}/recordEncoder.json 2>/dev/null")
    parts = out.split("---SEP---", 1)
    enc = parts[0].strip() or None
    renc_txt = parts[1].strip() if len(parts) > 1 else ""
    return enc, renc_txt


def _record_json_matches(renc_txt, want):
    """True iff the on-disk recordEncoder.json already carries the target VAAPI CQP settings."""
    if not renc_txt:
        return False
    try:
        got = json.loads(renc_txt)
    except Exception:  # noqa: BLE001
        return False
    return all(got.get(k) == want[k] for k in ("rate_control", "qp", "vaapi_device"))


def _obs_started_after_record_json(host, profile_dir):
    """True iff the RUNNING OBS was started AFTER the recordEncoder.json was written -- i.e. OBS
    actually BUILT the encoder from the vaapi config (OBS builds the Advanced-output encoder at
    startup from disk; a config written to disk while OBS runs does not rebuild it, #847). Without
    this, a disk that says vaapi while OBS is still running the pre-vaapi obs_x264 encoder would be
    misjudged 'live'. Fail-CLOSED (return False -> force an apply/restart) on any read hiccup."""
    base = f"~/.config/obs-studio/basic/profiles/{profile_dir}"
    rc, out = _ssh_capture(
        host, _ENSURE_XDG
        + "j=$(stat -c %Y " + base + "/recordEncoder.json 2>/dev/null); "
          "t=$(systemctl --user show imag-obs -p ActiveEnterTimestamp --value 2>/dev/null); "
          "o=$(date -d \"$t\" +%s 2>/dev/null); "
          "if [ -n \"$j\" ] && [ -n \"$o\" ] && [ \"$o\" -gt \"$j\" ]; then echo LIVE; else echo STALE; fi")
    return rc == 0 and "LIVE" in out


def ensure_rec_encoder(host, port, password):
    """Ensure the record encoder is the HW target AND live, applying the #847 ordering only when the
    pure apply-plan says a make-it-live restart is needed."""
    has_dgpu = detect_has_discrete_nvidia(host)
    available = _obs_available_encoders(host)
    target = select_rec_encoder(has_dgpu, available)
    profile_dir = _current_profile_dir(host, port, password)
    enc, renc_txt = _read_rec_encoder_config(host, profile_dir)
    want = imag_record_encoder.vaapi_record_encoder_settings()
    # renc_ok requires BOTH the correct on-disk settings AND that OBS actually booted with them
    # (built the vaapi encoder), so a disk-says-vaapi-but-OBS-still-runs-x264 window forces an apply.
    renc_ok = (_record_json_matches(renc_txt, want)
               and _obs_started_after_record_json(host, profile_dir))
    plan = imag_record_encoder.record_encoder_apply_plan(enc or "", target, renc_ok)
    print(f"[ensure-rec-encoder] host={host} profile={profile_dir} target={target} "
          f"current={enc!r} renc_ok={renc_ok} "
          f"available={'?' if available is None else sorted(available)} -> {plan}")
    if plan == "ok":
        print(f"[ensure-rec-encoder] record encoder already {target} + live — no restart")
        return

    base = f"~/.config/obs-studio/basic/profiles/{profile_dir}"
    # 1) WS RecEncoder FIRST (persists into OBS memory so its own shutdown save keeps the new value)
    obs = _ws_connect(host, port, password)
    try:
        obs.req("SetProfileParameter", {
            "parameterCategory": "AdvOut", "parameterName": "RecEncoder",
            "parameterValue": target}, ignore_err=True)
    finally:
        obs.ws.close()
    # 2) stop OBS through the USER unit (#847/#1015)
    rc, _ = _ssh_capture(host, _ENSURE_XDG + "systemctl --user stop imag-obs")
    print(f"[ensure-rec-encoder] stop imag-obs rc={rc}")
    time.sleep(3)
    # 3) write/remove recordEncoder.json while OBS is DOWN
    if target == imag_record_encoder.VAAPI_TEX_ENCODER:
        payload = json.dumps(want)
        rc, _ = _ssh_capture(host, f"cat > {base}/recordEncoder.json <<'JSON'\n{payload}\nJSON")
        print(f"[ensure-rec-encoder] wrote recordEncoder.json rc={rc}: {payload}")
    else:
        rc, _ = _ssh_capture(host, f"rm -f {base}/recordEncoder.json")
        print(f"[ensure-rec-encoder] removed recordEncoder.json (target {target}) rc={rc}")
    # 4) start OBS
    rc, _ = _ssh_capture(host, _ENSURE_XDG + "systemctl --user start imag-obs")
    print(f"[ensure-rec-encoder] start imag-obs rc={rc}")
    # 5) wait for WS back + verify read-back
    deadline = time.time() + 90
    live = None
    while time.time() < deadline:
        try:
            obs = Obs(host, port, password)
            try:
                live = obs.req("GetProfileParameter", {
                    "parameterCategory": "AdvOut", "parameterName": "RecEncoder"},
                    ignore_err=True).get("parameterValue")
            finally:
                obs.ws.close()
            break
        except Exception:  # noqa: BLE001 -- OBS still coming up; retry until the deadline
            time.sleep(3)
    if live is None:
        sys.exit(f"FAIL: imag OBS WebSocket never returned after the record-encoder restart "
                 f"(host={host}) — OBS may be down; check `systemctl --user status imag-obs`")
    if live != target:
        print(f"[ensure-rec-encoder] WARN: read-back RecEncoder={live!r} != target {target!r} — "
              "proceeding (the verdict's report-only lagged% will surface a stale encoder)")
    else:
        print(f"[ensure-rec-encoder] OK: record encoder is now {target}, live (restart applied)")


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
    ap.add_argument("--ensure-rec-encoder", action="store_true",
                    help="#1143: ensure the record encoder is the HW target (VAAPI-tex on Intel) and "
                         "LIVE — applies WS RecEncoder + recordEncoder.json + a USER-unit restart "
                         "ONLY when the disk config is not already the live target. Manages its own "
                         "OBS restart, so it is dispatched BEFORE the WS connection below.")
    ap.add_argument("--active-cams", default=None,
                    help="#1218: the active camera set (e.g. \"cam1 cam2 cam6\", from "
                         "CAMERA_ACTIVE_SET). Inactive cameras' NDI receivers are idled so imag stops "
                         "decoding them (the thermal-throttle root cause). A dev1 seed/enforce pass "
                         "ALSO writes it to the box's ~/.config/camera-box/imag-active-cams so the "
                         "next on-box --bootstrap reads a fresh copy. Omitted -> the on-box state "
                         "file, else baseline-heal (the pre-1218 behavior).")
    ap.add_argument("--enforce-ndi-policy", action="store_true",
                    help="#1218: apply the active-set NDI idle policy once over WS (the E2E/dev1 "
                         "reenforce pass) WITHOUT a full scene seed, then exit.")
    args = ap.parse_args()
    # #1218: resolve the active set (flag -> on-box state file -> None) for every mode below.
    active_cams = resolve_active_cams(args.active_cams)
    if args.ensure_rec_encoder:
        # #1143: ensure_rec_encoder stops/starts OBS itself, so it must NOT reuse a pre-opened WS.
        ensure_rec_encoder(args.host, args.port, args.password)
        return
    # #1218: a dev1 pass carrying an explicit --active-cams writes the one-line state file to the box
    # (or locally) so the next on-box --bootstrap self-heal reads a fresh copy. Only the WRITE modes
    # (seed / --enforce-ndi-policy) — never the read-only --verify-parity / --projector.
    if args.active_cams and args.active_cams.strip() and not (args.verify_parity or args.projector):
        _write_active_cams_state(args.host, args.active_cams)
    obs = Obs(args.host, args.port, args.password)
    if args.enforce_ndi_policy:
        # #1218: the dev1 reenforce pass — apply the active-set idle policy immediately, no reseed.
        statuses = enforce_ndi_active_policy(obs, active_cams)
        print("ndi policy (active=%s): %s"
              % (format_active_cams(active_cams) or "unknown(baseline-heal)",
                 ", ".join("CAM%d=%s" % (n, s) for n, s in sorted(statuses.items()))))
        return
    if args.projector:
        projector(obs, args.host)
    elif args.verify_parity:
        verify_parity(obs, active_cams)
    else:
        # #847: detect once per run -- never guessed, see detect_has_discrete_nvidia above.
        has_dgpu = detect_has_discrete_nvidia(args.host)
        seed_profile(obs, has_dgpu)  # #502/#847: Advanced profile BEFORE the video/scene seed
        seed(obs, active_cams)
        if BOOTSTRAP:
            # #866: fresh OBS instance -- force measurement burns OFF so a saved genlock_burn=true
            # can never survive a restart onto the live IMAG projection. Bootstrap-only (like the
            # #785 self-heal): a bare reseed must not clear a burn a gate run set mid-measurement.
            clear_measurement_burns(obs)


if __name__ == "__main__":
    main()
