#!/usr/bin/env python3
"""issue 1553 -- when the CG arm of the program-audio guard is only ADVISORY (ROZHODNUTÉ 25.11.2026).

WHY (issue 1553 comments 7174372378, 7174384065, 7174499060): the CG arm (program_audio_cg.overlay)
refuses every YouTube/FB test while SongPlayer plays, although on stream-snv the stream OBS program
tree carries ONE audio input, `mbc`, and CG audio can reach the stream only through the mbc mix, which
fohmixer can prove carries HAND 1 sync only. The owner's ruling: the CG arm is ADVISORY (logged, in the
payload, never FOREIGN/UNKNOWN, never latched) while BOTH hold:

  (a) the stream OBS program tree carries no audible input other than the program-audio input
      (STRIH_PROGRAM_AUDIO_INPUT of the stream box's fact file, `mbc` on stream-snv, never a literal):
      read over OBS-WS, read-only, through the obs_phase2 client over ONE session kept open -- the
      current program scene, its ENABLED items descending nested scenes and groups, plus the global
      audio devices (GetSpecialInputs, they mix into every output) and a scene transition whose kind
      may carry audio (a stinger); an input is audible when it has an audio track on
      (GetInputAudioTracks, mixers != 0) and is not muted (GetInputMute); an input without audio
      answers request code 604 and is not audible. Known limit: during a cut, the OUTGOING scene's
      inputs stay in the mix for the transition's length; the tree is re-read every 15 s;
  (b) fohmixer's mbc proof (fohmixer issue 267, `GET /api/mbc/stream-proof` on the FOH hub, LAN only)
      reads `ok` true AND `hand1_sync_only` true.

Either unreadable, stale or false = the strict fail-closed CG arm exactly as before (the probe's
`state()` says why). Config, all in the sampler's env file (`~/.config/camera-box/program-audio-sampler.env`):
  PROGRAM_AUDIO_MBC_PROOF_URL         the proof URL; absent/empty = no probe at all = strict
  PROGRAM_AUDIO_MBC_PROOF_TOKEN_FILE  a file holding the hub's automation token, sent as
                                      `Authorization: Bearer <token>`; absent/empty = strict. The
                                      token is never in git, argv or a log (read per request)
  PROGRAM_AUDIO_STREAM_BOX            the stream box's fleet name (default stream-snv): its host from
                                      obs_fleet_table, its program-audio input from its fact file
  PROGRAM_AUDIO_STREAM_OBS_PASSWORD   its obs-websocket password (default none: the rig runs no auth)

Rates and ages (documented bounds; pinned by tests):
  * the tree every TREE_INTERVAL_S (15 s); its last answer counts for TREE_MAX_AGE_S (45 s);
  * the mbc proof at most once per MBC_PROOF_INTERVAL_S (60 s, the owner's "at most once a minute"; a
    failed attempt counts); its age = the time since our read + the hub's own `age_ms` (the hub caches
    one walk per 30 s); over MBC_PROOF_MAX_AGE_S (150 s: two missed reads + the hub's cache) =
    unreadable. A missing / negative / non-numeric `age_ms` is unreadable too.
A failed read REPLACES the last good one (fail closed at once, never "the last good answer").
"""
from __future__ import annotations

import json
import math
import os
import re
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Callable, NamedTuple

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import dantesync_fleet as dsf  # noqa: E402  (the fact-file parser)
import obs_fleet_table as oft  # noqa: E402

URL_ENV = "PROGRAM_AUDIO_MBC_PROOF_URL"
TOKEN_FILE_ENV = "PROGRAM_AUDIO_MBC_PROOF_TOKEN_FILE"
STREAM_BOX_ENV = "PROGRAM_AUDIO_STREAM_BOX"
STREAM_PASSWORD_ENV = "PROGRAM_AUDIO_STREAM_OBS_PASSWORD"
STREAM_BOX_DEFAULT = "stream-snv"
STRIH_BOXES_DIR = Path(__file__).resolve().parent / "strih-boxes"
PROGRAM_INPUT_FACT = "STRIH_PROGRAM_AUDIO_INPUT"

TREE_INTERVAL_S = 15.0
TREE_MAX_AGE_S = 45.0
MBC_PROOF_INTERVAL_S = 60.0
MBC_PROOF_MAX_AGE_S = 150.0
MBC_PROOF_TIMEOUT_S = 20.0
OBS_REQUEST_TIMEOUT_S = 10.0
NO_AUDIO_CODE = 604  # obs-websocket RequestStatus::InvalidResourceState: "does not support audio"
SCENE_SOURCE_TYPE = "OBS_SOURCE_TYPE_SCENE"
THREAD_NAME = "program-audio-cg-advisory"


class Reading(NamedTuple):
    """One proof's last answer: ok True / False, None = unreadable; `why` names what was read or what
    failed; `at` = our monotonic time of the read; `extra_age_s` = the source's own age of the answer."""
    ok: bool | None
    why: str
    at: float | None = None
    extra_age_s: float = 0.0


class Advisory(NamedTuple):
    """The verdict for one window: `applies` = the CG arm is advisory now; `text` = the line the guard
    shows (applies) or why it is strict; the two proofs' ages in seconds (None: never read)."""
    applies: bool
    text: str
    tree_age_s: float | None = None
    mbc_proof_age_s: float | None = None


def advisory_text(program_input: str) -> str:
    return f"cg advisory (mbc proof: hand1 sync only, program tree: {program_input} only)"


# ------------------------------------------------------------------ (a) the program tree, pure

class Unreadable(Exception):
    """A proof cannot be read: the CG arm stays strict."""


def visible_inputs(fetch_items: Callable[[str, bool], list], scene: str) -> list[str]:
    """Every input an ENABLED item of `scene` renders, descending nested scenes and groups (a cycle is
    walked once), in first-seen order. `fetch_items(name, is_group)` returns the sceneItems."""
    out: list[str] = []
    seen: set[str] = set()

    def walk(name: str, is_group: bool) -> None:
        if name in seen:
            return
        seen.add(name)
        for item in fetch_items(name, is_group):
            if not bool(item.get("sceneItemEnabled", True)):
                continue
            src = item.get("sourceName")
            if not isinstance(src, str) or not src:
                raise Unreadable(f"a scene item of {name!r} has no source name")
            if item.get("sourceType") == SCENE_SOURCE_TYPE:
                walk(src, bool(item.get("isGroup")))
            elif src not in out:
                out.append(src)

    walk(scene, False)
    return out


def audible_inputs(names: list[str], audio_state: Callable[[str], tuple[bool, bool] | None]) -> list[str]:
    """The names that carry audio into the outputs: `audio_state(name)` = (any track on, muted), or
    None for an input without audio."""
    out = []
    for n in names:
        st = audio_state(n)
        if st is None:
            continue
        tracks_on, muted = st
        if tracks_on and not muted:
            out.append(n)
    return out


def tree_verdict(audible: list[str], program_input: str) -> Reading:
    others = [n for n in audible if n != program_input]
    if others:
        return Reading(False, "also " + ", ".join(repr(n) for n in others))
    return Reading(True, f"{program_input} only")


# ------------------------------------------------------------------ (b) the mbc proof, pure

def _number(v) -> float | None:
    return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v) else None


def _clean(v, limit: int = 120) -> str:
    """fohmixer text for our log and payload: control characters and whitespace runs collapsed,
    cut to `limit` characters."""
    text = " ".join("".join(c if c.isprintable() else " " for c in str(v)).split())
    return text if len(text) <= limit else text[:limit - 3] + "..."


def mbc_proof_verdict(payload) -> Reading:
    """fohmixer's answer -> Reading: ok AND hand1_sync_only true = True; ok false (its reason) =
    unreadable; hand1_sync_only false = False (the open tracks named); `extra_age_s` = its age_ms."""
    if not isinstance(payload, dict):
        return Reading(None, "the mbc proof is not a JSON object")
    if payload.get("ok") is not True:
        reason = payload.get("reason")
        return Reading(None, "the mbc proof reads ok=false" + (f" ({_clean(reason)})" if reason else ""))
    age_ms = _number(payload.get("age_ms"))
    if age_ms is None or age_ms < 0:
        return Reading(None, f"the mbc proof has no valid age_ms ({_clean(repr(payload.get('age_ms')))})")
    h1 = payload.get("hand1_sync_only")
    if h1 is True:
        return Reading(True, "hand1 sync only", extra_age_s=age_ms / 1000.0)
    if h1 is False:
        opened = payload.get("open")
        return Reading(False, "not hand1 sync only" + (f" (open {_clean(opened)})" if opened else ""),
                       extra_age_s=age_ms / 1000.0)
    return Reading(None, f"the mbc proof has no hand1_sync_only ({_clean(repr(h1))})")


# ------------------------------------------------------------------ the decision, pure

def _age(r: Reading | None, now: float) -> float | None:
    if r is None or r.at is None:
        return None
    return max(0.0, now - r.at) + r.extra_age_s


def _grade(r: Reading | None, now: float, max_age_s: float, never: str) -> tuple[bool, str]:
    if r is None:
        return False, never
    if r.ok is None:
        return False, r.why
    if r.ok is False:
        return False, r.why
    age = _age(r, now)
    if age is None or age > max_age_s:
        return False, f"stale ({age:.0f} s > {max_age_s:g} s)" if age is not None else never
    return True, r.why


def decide(tree: Reading | None, mbc: Reading | None, now: float, program_input: str) -> Advisory:
    t_ok, t_why = _grade(tree, now, TREE_MAX_AGE_S, "not read yet")
    m_ok, m_why = _grade(mbc, now, MBC_PROOF_MAX_AGE_S, "not read yet")
    ages = (_age(tree, now), _age(mbc, now))
    if t_ok and m_ok:
        return Advisory(True, advisory_text(program_input), *ages)
    return Advisory(False, f"cg strict (mbc proof: {m_why}, program tree: {t_why})", *ages)


# ------------------------------------------------------------------ the live readers

def stream_box() -> str:
    return os.environ.get(STREAM_BOX_ENV) or STREAM_BOX_DEFAULT


def program_input_for(box: str, boxes_dir: Path | None = None) -> str:
    """STRIH_PROGRAM_AUDIO_INPUT of the box's fact file. Raises ValueError when absent or `none`."""
    path = (boxes_dir or STRIH_BOXES_DIR) / f"{box}.env"
    try:
        facts = dsf.fact_file(path)
    except OSError as exc:
        raise ValueError(f"the stream box fact file {path} is unreadable: {exc}") from exc
    name = facts.get(PROGRAM_INPUT_FACT, "")
    if not name or name == "none":
        raise ValueError(f"{path} has no {PROGRAM_INPUT_FACT}")
    return name


_CODE_RE = re.compile(r"'code':\s*(\d+)")


# Scene transitions that carry no audio (OBS's built-in kinds); any other (a stinger, a plugin) may mix
# its own media into the program during a cut, so it reads as another audible input.
SILENT_TRANSITION_KINDS = frozenset({"cut_transition", "fade_transition", "swipe_transition",
                                     "slide_transition", "fade_to_color_transition", "wipe_transition"})


def transition_audio(transition: dict) -> str | None:
    """The current scene transition as an audible 'input' when its kind may carry audio, else None."""
    kind = transition.get("transitionKind")
    if not isinstance(kind, str) or not kind:
        raise Unreadable("GetCurrentSceneTransition answered no transitionKind")
    if kind in SILENT_TRANSITION_KINDS:
        return None
    return f"transition {transition.get('transitionName')!r} ({kind})"


def read_program_tree_over(ws, rpc) -> list[str]:
    """The audible inputs of the program tree over one open session (`rpc(ws, type, data)`): the
    program scene's enabled items through nested scenes and groups, the global audio devices, and a
    transition that may carry audio. Raises on any failure."""
    scene = rpc(ws, "GetCurrentProgramScene").get("currentProgramSceneName")
    if not scene:
        raise Unreadable("no current program scene")

    def fetch(name, is_group):
        req = "GetGroupSceneItemList" if is_group else "GetSceneItemList"
        return rpc(ws, req, {"sceneName": name}).get("sceneItems", [])

    names = visible_inputs(fetch, scene)
    for special in (rpc(ws, "GetSpecialInputs") or {}).values():
        if isinstance(special, str) and special and special not in names:
            names.append(special)

    def audio_state(name):
        try:
            tracks = rpc(ws, "GetInputAudioTracks", {"inputName": name}).get("inputAudioTracks") or {}
        except RuntimeError as exc:
            m = _CODE_RE.search(str(exc))
            if m and int(m.group(1)) == NO_AUDIO_CODE:
                return None
            raise
        muted = rpc(ws, "GetInputMute", {"inputName": name}).get("inputMuted")
        if not isinstance(muted, bool):
            raise Unreadable(f"GetInputMute of {name!r} answered no inputMuted")
        return any(bool(v) for v in tracks.values()), muted

    audible = audible_inputs(names, audio_state)
    trans = transition_audio(rpc(ws, "GetCurrentSceneTransition") or {})
    return audible + ([trans] if trans else [])


class ProgramTreeReader:
    """Reads the tree over ONE obs-websocket session kept open between reads (review: a session per
    read would put ~5,800 connect/close pairs a day into the stream OBS log). Any failure closes it;
    the next read reconnects. Read-only: every request goes through obs_phase2's `_rpc` (its
    production-scene guard), the session subscribes to no events (obs_phase2._conn)."""

    def __init__(self, host: str, password: str = ""):
        self.host = host
        self.password = password
        self._ws = None

    def _client(self):
        try:
            import obs_phase2  # lazy: websocket-client only here, so the pure parts import without it
        except (SystemExit, ImportError) as exc:  # obs_phase2 exits on a missing websocket-client: strict
            raise Unreadable(f"the OBS-WS client cannot load ({exc})") from None
        return obs_phase2

    def __call__(self) -> list[str]:
        client = self._client()

        def rpc(ws, rtype, data=None):
            return client._rpc(ws, rtype, data, timeout_s=OBS_REQUEST_TIMEOUT_S) or {}

        try:
            if self._ws is None:
                self._ws = client._conn(self.host, self.password)
                self._ws.settimeout(OBS_REQUEST_TIMEOUT_S)
            return read_program_tree_over(self._ws, rpc)
        except BaseException:
            self.close()
            raise

    def close(self) -> None:
        ws, self._ws = self._ws, None
        if ws is not None:
            try:
                ws.close()
            except Exception:  # noqa: BLE001 -- a dead session is dropped either way
                pass


def read_program_tree(host: str, password: str = "") -> list[str]:
    """One read over a fresh session (ops, tests); the probe keeps a ProgramTreeReader instead."""
    reader = ProgramTreeReader(host, password)
    try:
        return reader()
    finally:
        reader.close()


def read_token(path: str) -> str:
    try:
        token = Path(path).read_text(encoding="utf-8").strip()
    except OSError as exc:
        raise Unreadable(f"the mbc proof token file is unreadable ({exc.__class__.__name__})") from exc
    if not token or any(c.isspace() for c in token):
        raise Unreadable("the mbc proof token file holds no single token")
    return token


def fetch_mbc_proof(url: str, token_file: str, timeout_s: float = MBC_PROOF_TIMEOUT_S):
    """GET the proof with the bearer token; the parsed JSON. Raises Unreadable naming the failure
    (never the token)."""
    req = urllib.request.Request(url, headers={"Authorization": "Bearer " + read_token(token_file),
                                               "Accept": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout_s) as resp:
            body = resp.read()
    except urllib.error.HTTPError as exc:
        raise Unreadable(f"the mbc proof answered HTTP {exc.code}") from exc
    except (urllib.error.URLError, OSError) as exc:
        raise Unreadable(f"the mbc proof is unreachable ({getattr(exc, 'reason', exc)})") from exc
    try:
        return json.loads(body)
    except ValueError as exc:
        raise Unreadable("the mbc proof is not JSON") from exc


# ------------------------------------------------------------------ the thread

def _why(exc: BaseException) -> str:
    return str(exc) if isinstance(exc, (Unreadable, ValueError)) else f"read failed: {exc!r}"


class AdvisoryProbe:
    """Reads both proofs in its own thread (a WS session or an HTTP call never stalls the consumer);
    `state(now)` is the pure decision over the cached readings. `read_tree()` -> the audible input
    names, `read_mbc()` -> the proof's JSON; both raise when unreadable."""

    def __init__(self, program_input: str, read_tree: Callable[[], list[str]],
                 read_mbc: Callable[[], object], *, mono: Callable[[], float] = time.monotonic,
                 tick_s: float = 1.0):
        self.program_input = program_input
        self._read_tree = read_tree
        self._read_mbc = read_mbc
        self._mono = mono
        self._tick_s = tick_s
        self._lock = threading.Lock()
        self.tree: Reading | None = None
        self.mbc: Reading | None = None
        self._tree_try: float | None = None
        self._mbc_try: float | None = None
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def poll_once(self) -> None:
        now = self._mono()
        if self._tree_try is None or now - self._tree_try >= TREE_INTERVAL_S:
            self._tree_try = now
            try:  # the answer is as old as the read's START (a slow read never looks fresh)
                r = tree_verdict(list(self._read_tree()), self.program_input)._replace(at=now)
            except BaseException as exc:  # noqa: BLE001 -- unreadable = strict, never a dead thread
                r = Reading(None, "unreadable: " + _why(exc), now)
            with self._lock:
                self.tree = r
        now = self._mono()
        if self._mbc_try is None or now - self._mbc_try >= MBC_PROOF_INTERVAL_S:
            self._mbc_try = now
            try:
                r = mbc_proof_verdict(self._read_mbc())._replace(at=now)
            except BaseException as exc:  # noqa: BLE001 -- unreadable = strict, never a dead thread
                r = Reading(None, _why(exc), now)
            with self._lock:
                self.mbc = r

    def state(self, now: float | None = None) -> Advisory:
        now = self._mono() if now is None else now
        with self._lock:
            tree, mbc = self.tree, self.mbc
        if self._thread is not None and not self._thread.is_alive() and not self._stop.is_set():
            return Advisory(False, "cg strict (the advisory probe thread stopped)")
        return decide(tree, mbc, now, self.program_input)

    def _run(self) -> None:
        while not self._stop.is_set():
            self.poll_once()
            self._stop.wait(self._tick_s)

    def start(self) -> None:
        self._thread = threading.Thread(target=self._run, name=THREAD_NAME, daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        close = getattr(self._read_tree, "close", None)
        if close is not None:
            close()


def from_env(env=None, *, boxes_dir: Path | None = None) -> AdvisoryProbe | None:
    """The probe the config asks for, NOT started; None (strict, no probe) when the proof URL or the
    token file is not configured. Raises ValueError when the stream box is unknown (fail loud)."""
    env = os.environ if env is None else env
    url = (env.get(URL_ENV) or "").strip()
    token_file = (env.get(TOKEN_FILE_ENV) or "").strip()
    if not url or not token_file:
        return None
    box = env.get(STREAM_BOX_ENV) or STREAM_BOX_DEFAULT
    host = oft.fleet_host(box)
    if not host:
        raise ValueError(f"the stream box {box!r} is not in the OBS fleet table")
    program_input = program_input_for(box, boxes_dir)
    password = env.get(STREAM_PASSWORD_ENV, "")
    return AdvisoryProbe(program_input, ProgramTreeReader(host, password),
                         lambda: fetch_mbc_proof(url, token_file))


def start_line(probe: AdvisoryProbe | None) -> str:
    if probe is None:
        return (f"program-audio sampler: CG arm strict (no {URL_ENV} / {TOKEN_FILE_ENV} configured: "
                "no advisory probe)")
    return (f"program-audio sampler: CG arm advisory while the stream OBS program tree has no audible "
            f"input but {probe.program_input!r} (read every {TREE_INTERVAL_S:g} s, valid "
            f"{TREE_MAX_AGE_S:g} s) AND the mbc proof reads ok + hand1_sync_only (read at most every "
            f"{MBC_PROOF_INTERVAL_S:g} s, valid {MBC_PROOF_MAX_AGE_S:g} s incl. its age_ms); else strict")
