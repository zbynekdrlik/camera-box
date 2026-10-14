#!/usr/bin/env python3
"""issue 1432 -- the authenticated WRITE side of the dev1 rig lease, for a holder on another host.

WHY: restreamer's OBS-streaming CI runs on the stream box (`[self-hosted, windows, stream-lan]`) and
could only READ the lease (GET :8890/rig-lease.json, issue 1277). It never TOOK it, so its 45-min CI
stream on 14.10.2026 23:33 was invisible to camera-box as a holder: the release E2E saw only an
anonymous `outputActive=true` and died after its 30-min busy budget. This module is the logic behind
three POST routes on scripts/rig-lease-server.py:

  POST /rig-lease/acquire    {repo, run_id, run_url, job, expected_secs 60..5400}
       200 {acquired: true, lease_id, holder, expected_release_at}
       409 {acquired: false, holder, heartbeat_age_s}      (someone else holds a live lease)
  POST /rig-lease/heartbeat  {lease_id}    every 30 s
       200 {refreshed: true, expected_release_at}  -- rolls the release to now + 15 min
       404 {error: "not_holder"}   409 {error: "over_hold"}   503 {error: "lease_io"}
  POST /rig-lease/release    {lease_id}    idempotent
       200 {released: true|false}

  400 on bad input, 401 without a valid `Authorization: Bearer <token>`.

The lock itself is NEVER reimplemented here: every write runs the existing functions of
scripts/lib/rig-lease.sh (rig_lease_acquire / rig_lease_refresh_if_mine / rig_lease_release) in a
`bash` subprocess, arguments as argv only, with RIG_LEASE_DIR set to the server's lease dir. So the
mkdir lock, the stale reclaim and the issue-857 atomic release stay ONE implementation.

A remote holder is written with three extra holder.json keys (rig_lease_write_holder's optional env):
  lease_id    a random 128-bit hex token, returned only to the acquirer, never served on GET
  stale_secs  180: it beats every 30 s, so 3 min of silence makes it reclaimable
  max_hold_s  = expected_secs: the hold CEILING the heartbeat enforces (issue 1383)
Its declared release is now + min(expected_secs, 900) -- the rolling look-ahead of the issue-1383
new-holder checklist, so a camera-box gate WAITS for it instead of failing fast.

Auth: the bearer is compared (hmac.compare_digest) with the stripped content of a token file
(`RIG_LEASE_TOKEN_FILE`, default ~/.config/camera-box/rig-lease-token). The file must be a regular
file owned by this user with no group/other permission bits; anything else -- absent, empty, wrong
mode -- refuses EVERY write (401). The token is read per request, so rotating it needs no restart.
"""
from __future__ import annotations

import hmac
import json
import os
import re
import secrets
import stat
import subprocess
from datetime import datetime, timedelta, timezone

import rig_lease_state as rls

LIB = os.path.join(os.path.dirname(os.path.abspath(__file__)), "lib", "rig-lease.sh")

MAX_BODY = 4096
REMOTE_STALE_SECS = 180
LOOKAHEAD_S = 900
MIN_EXPECTED_S = 60
MAX_EXPECTED_S = 5400
BASH_TIMEOUT_S = 30

WRITE_PATHS = ("/rig-lease/acquire", "/rig-lease/heartbeat", "/rig-lease/release")

_REPO = re.compile(r"[A-Za-z0-9._-]{1,64}/[A-Za-z0-9._-]{1,100}", re.ASCII)
_RUN_ID = re.compile(r"[A-Za-z0-9._-]{1,64}", re.ASCII)
_JOB = re.compile(r"[A-Za-z0-9 ._:/()-]{1,100}", re.ASCII)
_RUN_URL = re.compile(r"https://[!-~]{1,300}", re.ASCII)
_LEASE_ID = re.compile(r"[0-9a-f]{32}", re.ASCII)


class Reply(Exception):
    """A finished HTTP answer: status + JSON payload."""

    def __init__(self, status: int, payload: dict):
        super().__init__(status)
        self.status = status
        self.payload = payload


def default_token_file() -> str:
    raw = os.environ.get("RIG_LEASE_TOKEN_FILE", "")
    return raw or os.path.join(os.path.expanduser("~"), ".config", "camera-box", "rig-lease-token")


def read_token(path: str) -> str | None:
    """The configured token, or None when the file is unusable (every write is then refused)."""
    try:
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    except OSError:
        return None
    try:
        st = os.fstat(fd)
        if not stat.S_ISREG(st.st_mode) or st.st_uid != os.getuid() or st.st_mode & 0o077:
            return None
        data = os.read(fd, 4096)
    finally:
        os.close(fd)
    token = data.decode("utf-8", "replace").strip()
    return token or None


def authorized(header: str | None, token_file: str) -> bool:
    token = read_token(token_file)
    if token is None or not header or not header.startswith("Bearer "):
        return False
    return hmac.compare_digest(header[len("Bearer "):].strip().encode(), token.encode())


def parse_body(raw: bytes) -> dict:
    try:
        body = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        raise Reply(400, {"error": "bad_json"})
    if not isinstance(body, dict):
        raise Reply(400, {"error": "bad_json"})
    return body


def _field(body: dict, name: str, pattern: re.Pattern) -> str:
    value = body.get(name)
    if not isinstance(value, str) or not pattern.fullmatch(value):
        raise Reply(400, {"error": "bad_field", "field": name})
    return value


def _expected_secs(body: dict) -> int:
    value = body.get("expected_secs")
    if isinstance(value, bool) or not isinstance(value, int) or not MIN_EXPECTED_S <= value <= MAX_EXPECTED_S:
        raise Reply(400, {"error": "bad_field", "field": "expected_secs"})
    return value


def _bash(lease_dir: str, script: str, args: list, extra_env: dict | None = None):
    env = dict(os.environ, RIG_LEASE_DIR=lease_dir)
    env.update(extra_env or {})
    try:
        return subprocess.run(
            ["bash", "-c", 'set -uo pipefail; . "$0"; ' + script, LIB, *args],
            capture_output=True, text=True, env=env, timeout=BASH_TIMEOUT_S,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise Reply(503, {"error": "lease_io", "detail": type(exc).__name__})


def _read_holder(lease_dir: str):
    try:
        with open(os.path.join(lease_dir, "holder.json"), encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError):
        return None
    return data if isinstance(data, dict) else None


def _public_holder(holder: dict | None):
    if holder is None:
        return None
    return {field: holder.get(field) for field in rls._HOLDER_FIELDS}


def acquire(lease_dir: str, body: dict, stale_secs: int, now: datetime | None = None) -> Reply:
    repo = _field(body, "repo", _REPO)
    run_id = _field(body, "run_id", _RUN_ID)
    run_url = _field(body, "run_url", _RUN_URL)
    job = _field(body, "job", _JOB)
    expected = _expected_secs(body)
    now = now or datetime.now(timezone.utc)
    release_at = rls.format_ts(now + timedelta(seconds=min(expected, LOOKAHEAD_S)))
    lease_id = secrets.token_hex(16)
    r = _bash(
        lease_dir, 'rig_lease_acquire "$@"',
        [repo, run_id, run_url, job, release_at, str(stale_secs)],
        {
            "RIG_LEASE_HOLDER_LEASE_ID": lease_id,
            "RIG_LEASE_HOLDER_STALE_SECS": str(REMOTE_STALE_SECS),
            "RIG_LEASE_HOLDER_MAX_HOLD_SECS": str(expected),
        },
    )
    holder = _read_holder(lease_dir)
    if r.returncode == 0 and holder is not None and holder.get("lease_id") == lease_id:
        return Reply(200, {
            "acquired": True,
            "lease_id": lease_id,
            "holder": _public_holder(holder),
            "expected_release_at": holder.get("expected_release_at"),
            "reclaimed": "RIG_LEASE_RECLAIMED" in r.stdout,
        })
    if r.returncode == 1 and "RIG_LEASE_HELD_BY=" in r.stdout:
        state = rls.lease_state(lease_dir, datetime.now(timezone.utc), stale_secs)
        return Reply(409, {
            "acquired": False,
            "holder": state.get("holder"),
            "heartbeat_age_s": state.get("heartbeat_age_s"),
        })
    raise Reply(503, {"error": "lease_io", "detail": "acquire rc=%d" % r.returncode})


def _owned_holder(lease_dir: str, body: dict):
    lease_id = _field(body, "lease_id", _LEASE_ID)
    holder = _read_holder(lease_dir)
    if holder is None or not hmac.compare_digest(str(holder.get("lease_id", "")), lease_id):
        return None
    return holder


def heartbeat(lease_dir: str, body: dict) -> Reply:
    holder = _owned_holder(lease_dir, body)
    if holder is None:
        return Reply(404, {"error": "not_holder"})
    max_hold = holder.get("max_hold_s")
    if isinstance(max_hold, bool) or not isinstance(max_hold, int) or max_hold <= 0:
        max_hold = MAX_EXPECTED_S
    r = _bash(
        lease_dir, 'rig_lease_refresh_if_mine "$@"',
        [str(holder.get("repo", "")), str(holder.get("run_id", "")), str(LOOKAHEAD_S)],
        {"RIG_LEASE_MAX_HOLD_SECS": str(max_hold)},
    )
    if r.returncode == 0:
        match = re.search(r"expected_release_at=(\S+)", r.stdout)
        return Reply(200, {"refreshed": True, "expected_release_at": match.group(1) if match else None})
    if r.returncode == 1:
        return Reply(404, {"error": "not_holder"})
    if r.returncode == 3:
        return Reply(409, {"error": "over_hold"})
    raise Reply(503, {"error": "lease_io", "detail": "heartbeat rc=%d" % r.returncode})


def release(lease_dir: str, body: dict) -> Reply:
    holder = _owned_holder(lease_dir, body)
    if holder is None:
        return Reply(200, {"released": False})
    r = _bash(lease_dir, 'rig_lease_release "$@"', [str(holder.get("run_id", ""))])
    if r.returncode != 0:
        raise Reply(503, {"error": "lease_io", "detail": "release rc=%d" % r.returncode})
    still = _read_holder(lease_dir)
    return Reply(200, {"released": still is None or still.get("lease_id") != holder.get("lease_id")})


def handle(path: str, auth_header: str | None, raw_body: bytes | None, lease_dir: str,
           stale_secs: int, token_file: str) -> Reply:
    """One POST: the route, the auth, the body, the operation. Never raises."""
    try:
        if path not in WRITE_PATHS:
            return Reply(404, {"error": "not_found"})
        if not authorized(auth_header, token_file):
            return Reply(401, {"error": "unauthorized"})
        if raw_body is None:
            return Reply(400, {"error": "bad_length"})
        body = parse_body(raw_body)
        if path == "/rig-lease/acquire":
            return acquire(lease_dir, body, stale_secs)
        if path == "/rig-lease/heartbeat":
            return heartbeat(lease_dir, body)
        return release(lease_dir, body)
    except Reply as reply:
        return reply
