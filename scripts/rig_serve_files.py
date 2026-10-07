#!/usr/bin/env python3
"""issue 1404 -- the files the dev1 rig-lease server (:8890) serves next to the lease, stdlib only.

Two dev1 writers keep one file each in the SERVE dir, and `scripts/rig-lease-server.py` serves
them read-only:

  rig-qpsk-markers.csv  <- scripts/rig_marker_mirror.py (the --user service rig-marker-mirror,
                           one ssh `tail -F` stream from cam2): cam2's `/run/rig-qpsk-markers.csv`,
                           complete rows only. Restreamer's gate fetches it from
                           `http://dev1:8890/rig-qpsk-markers.csv` and never needs the fleet ssh
                           credentials.
  program-audio.json    <- scripts/program_audio_sampler.py (a --user service): the verdict on
                           whether the stream program audio is measurement-only (the YouTube
                           channel guard, `scripts/program_audio_guard.py`).

The serve dir:
  * defaults to `$XDG_RUNTIME_DIR/rig-lease-serve` (`/run/user/<uid>`: tmpfs, 0700, this user only),
    `$RIG_LEASE_SERVE_DIR` overrides it for the server and both writers alike. tmpfs: the mirror
    rewrites a multi-MB file every 10 s, which must not wear dev1's SSD. 0700: no other account on
    dev1 can plant a file the guard would trust;
  * is NEVER the lease dir or inside it: the lease dir's mere existence means `held=true`
    (`scripts/rig_lease_state.py`), so a writer's `mkdir -p` there would fake a held lease;
  * is served only file by file: a file another user owns is never served (`owned_by_me`).

This module is stdlib-only on purpose: the lease server is a coordination endpoint and must not
gain a numpy dependency (the sampler's analysis lives in `program_audio.py`).

`ReadOnlyHandler` is the one response framing of the two read-only servers that serve these files:
the dev1 rig-lease server and the program-audio sampler's own endpoint (`program_audio_http.py`,
host-agnostic since the sampler moves off dev1; issue 1404 design 6037613222).
"""
from __future__ import annotations

import json
import os
import stat
import tempfile
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler

SERVE_DIR_ENV = "RIG_LEASE_SERVE_DIR"
SERVE_DIR_NAME = "rig-lease-serve"
DEFAULT_LEASE_DIR = "/var/tmp/rig-lease"  # scripts/lib/rig-lease.sh: ${RIG_LEASE_DIR:-/var/tmp/rig-lease}
MARKERS_NAME = "rig-qpsk-markers.csv"
PROGRAM_AUDIO_NAME = "program-audio.json"


def default_serve_dir() -> str:
    if os.environ.get(SERVE_DIR_ENV):
        return os.environ[SERVE_DIR_ENV]
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.geteuid()}"
    return os.path.join(runtime, SERVE_DIR_NAME)


def default_lease_dir() -> str:
    """An EMPTY $RIG_LEASE_DIR means the default, exactly like the bash lease library."""
    return os.environ.get("RIG_LEASE_DIR") or DEFAULT_LEASE_DIR


def serve_dir_inside_lease(serve_dir: str, lease_dir: str) -> str | None:
    """A reason when `serve_dir` is the lease dir or lies inside it, else None."""
    serve = os.path.realpath(serve_dir)
    lease = os.path.realpath(lease_dir)
    if serve == lease or serve.startswith(lease + os.sep):
        return (f"serve dir {serve_dir} is (inside) the lease dir {lease_dir} -- "
                "its existence means held=true")
    return None


def owned_by_me(st: os.stat_result) -> bool:
    return st.st_uid == os.geteuid()


def serve_dir_problem(serve_dir: str, lease_dir: str) -> str | None:
    """Why a WRITER must not use `serve_dir` (inside the lease dir; an existing path that is not a
    directory, owned by another user, or writable by group/others), else None."""
    inside = serve_dir_inside_lease(serve_dir, lease_dir)
    if inside:
        return inside
    if not os.path.lexists(serve_dir):
        return None
    st = os.stat(serve_dir)
    if not stat.S_ISDIR(st.st_mode):
        return f"serve dir {serve_dir} is not a directory"
    if not owned_by_me(st):
        return f"serve dir {serve_dir} is owned by uid {st.st_uid}, not this user ({os.geteuid()})"
    if st.st_mode & 0o022:
        return f"serve dir {serve_dir} is writable by group/others (mode {stat.S_IMODE(st.st_mode):o})"
    return None


def ensure_serve_dir(serve_dir: str, lease_dir: str) -> None:
    """Create `serve_dir` 0700 if missing; raise ValueError with the reason when it is unusable."""
    problem = serve_dir_problem(serve_dir, lease_dir)
    if problem:
        raise ValueError(problem)
    os.makedirs(serve_dir, mode=0o700, exist_ok=True)
    problem = serve_dir_problem(serve_dir, lease_dir)
    if problem:
        raise ValueError(problem)


def format_ts_utc(dt: datetime) -> str:
    """`2026-10-06T19:30:02.123Z` -- UTC, millisecond resolution."""
    dt = dt.astimezone(timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%S.") + f"{dt.microsecond // 1000:03d}Z"


def parse_ts_utc(value) -> datetime | None:
    """Parse `format_ts_utc`'s form (or a whole-second `...Z`); None for anything else."""
    if not isinstance(value, str) or not value.endswith("Z"):
        return None
    for fmt in ("%Y-%m-%dT%H:%M:%S.%fZ", "%Y-%m-%dT%H:%M:%SZ"):
        try:
            return datetime.strptime(value, fmt).replace(tzinfo=timezone.utc)
        except ValueError:
            continue  # try the next accepted form; no form matching returns None below
    return None


def write_bytes_atomic(path: str, data: bytes, mode: int = 0o644) -> None:
    """Write `data` to `path` via a temp file in the SAME directory + fsync + rename, so a reader
    (the lease server) sees the old complete file or the new complete file, never a partial one.
    The temp file is removed on any failure; the error propagates (fail loud)."""
    directory = os.path.dirname(os.path.abspath(path))
    fd, tmp = tempfile.mkstemp(prefix=f".{os.path.basename(path)}.", dir=directory)
    try:
        with os.fdopen(fd, "wb") as fh:
            fh.write(data)
            fh.flush()
            os.fsync(fh.fileno())
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    except BaseException:
        if os.path.lexists(tmp):
            os.unlink(tmp)
        raise


def read_mirror(path: str, now_epoch: float) -> tuple[bytes, int] | None:
    """The mirrored file's bytes + its age in whole seconds (now - mtime = since the mirror last
    received new rows), or None while it is absent or owned by another user. Bytes, owner and
    mtime come from the SAME open file (fstat), so a concurrent rename cannot mix two files. Any
    other OSError (a directory in its place, a permission error) propagates to the caller."""
    try:
        fh = open(path, "rb")
    except FileNotFoundError:
        return None  # absent: the server answers 404 (the documented contract)
    with fh:
        st = os.fstat(fh.fileno())
        if not owned_by_me(st):
            return None
        data = fh.read()
    return data, max(0, int(now_epoch - st.st_mtime))


def _unknown(reason: str) -> dict:
    return {
        "schema": 1, "ts_utc": None, "age_s": None, "verdict": "UNKNOWN", "rms_dbfs": None,
        "outside_band_pct": None, "window_s": None, "last_foreign_ts_utc": None,
        "last_foreign_age_s": None, "reason": reason,
    }


def _age(now: datetime, value) -> float | None:
    ts = parse_ts_utc(value)
    return None if ts is None else round((now - ts).total_seconds(), 1)


def program_audio_response(path: str, now: datetime) -> dict | None:
    """The program-audio payload as served: `age_s` and `last_foreign_age_s` recomputed at request
    time from the payload's own `ts_utc` / `last_foreign_ts_utc` (one clock, dev1's, so a consumer
    on another host never compares two clocks, and a sampler that stopped writing reads stale,
    never fresh). None while the file is absent. Anything else that is not a payload this user
    wrote (an OS error, not JSON, another owner) is served FAIL-CLOSED as UNKNOWN with `age_s`
    null; an unparseable `ts_utc` keeps the verdict with `age_s` null (a consumer reads stale). A
    MEASUREMENT without a marker chain (a sampler older than the issue-1404 marker requirement) is
    served as UNKNOWN with its ages kept."""
    try:
        with open(path, "rb") as fh:
            st = os.fstat(fh.fileno())
            raw = fh.read()
    except FileNotFoundError:
        return None  # absent: the server answers 404 (the documented contract)
    except OSError as exc:
        return _unknown(f"program-audio.json unreadable: {exc}")
    if not owned_by_me(st):
        return _unknown(f"program-audio.json has another owner (uid {st.st_uid}) -- not trusted")
    try:
        payload = json.loads(raw)
        if not isinstance(payload, dict):
            raise ValueError("not a JSON object")
    except ValueError as exc:
        return _unknown(f"program-audio.json unreadable: {exc}")
    payload["age_s"] = _age(now, payload.get("ts_utc"))
    payload["last_foreign_age_s"] = _age(now, payload.get("last_foreign_ts_utc"))
    if payload.get("verdict") == "MEASUREMENT" and not _is_count(payload.get("marker_chain")):
        # issue 1404 (ROZHODNUTÉ 6026826572): MEASUREMENT needs the QPSK marker chain. Without one
        # the payload comes from a sampler older than that requirement (it keeps running after a
        # pull until restarted). Refused HERE, the one place every consumer reads (the camera-box
        # guard and restreamer's own reader); the ages stay, so a stale reading still reads stale.
        payload["verdict"] = "UNKNOWN"
        payload["reason"] = ("MEASUREMENT without a marker chain (a sampler older than the marker "
                             "requirement -- build the QPSK shim and restart program-audio-sampler)")
    return payload


def _is_count(v) -> bool:
    return isinstance(v, int) and not isinstance(v, bool) and v >= 0


class ReadOnlyHandler(BaseHTTPRequestHandler):
    """The response framing both read-only servers share (the dev1 rig-lease server and the
    program-audio sampler's own endpoint): GET and HEAD route through ONE `_handle()` the subclass
    defines (so the two can never drift on which paths exist; HEAD suppresses only the body), a
    query string is stripped before matching, and any other method is the base class's 501 -- never
    a write. A subclass sets `server_version` and `log_message`."""

    # Suppress the interpreter version from the Server: response header (BaseHTTPRequestHandler's
    # version_string() concatenates server_version + " " + sys_version) -- no reason to advertise
    # the exact Python patch version to an unauthenticated caller.
    sys_version = ""
    # A client that connects and sends nothing (or stalls mid-request) is dropped after this many
    # seconds instead of holding a server thread forever: both servers listen on 0.0.0.0, the
    # sampler's on a production box with no firewall (issue 1404 review round 3).
    timeout = 10

    def _handle(self):  # pragma: no cover -- every subclass defines its routes
        raise NotImplementedError

    def _request_path(self) -> str:
        # Strip a query string before matching -- `GET /rig-lease.json?t=1` (a common client-side
        # cache-buster) must still hit the real route, not fall through to 404 (which would make
        # restreamer's consumer contract fail-open and silently drop the lease check).
        return self.path.split("?", 1)[0]

    def _send(self, status: int, content_type: str, body: bytes, *, no_store: bool = False,
              extra_headers: tuple = ()) -> None:
        # The WHOLE response (status line + headers + body) is wrapped in ONE try/except -- a
        # client that disconnects between send_response() and end_headers() would otherwise raise
        # an unguarded BrokenPipeError/ConnectionResetError (only the body write used to be
        # guarded), which socketserver logs as a traceback even though it is not a real fault.
        try:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            if no_store:
                self.send_header("Cache-Control", "no-store")
            for name, value in extra_headers:
                self.send_header(name, value)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):  # airuleset:script-ok a client hang-up mid-response is not a server fault
            return

    def do_GET(self):
        self._handle()

    def do_HEAD(self):
        # A cheap liveness probe an external checker can use without paying for a JSON body --
        # routes through the SAME path matching as do_GET (_handle() suppresses the body write via
        # self.command == "HEAD" inside _send()), so the two can never drift on which paths are
        # recognized.
        self._handle()
