#!/usr/bin/env python3
"""issue 1404 -- the files the dev1 rig-lease server (:8890) serves next to the lease, stdlib only.

Two dev1 writers drop one file each into the SERVE dir, and `scripts/rig-lease-server.py` serves
them read-only:

  rig-qpsk-markers.csv  <- scripts/rig-marker-mirror.sh (a --user timer, every 10 s): cam2's
                           `/run/rig-qpsk-markers.csv`, byte-identical. Restreamer's gate fetches
                           it from `http://dev1:8890/rig-qpsk-markers.csv` and never needs the
                           fleet ssh credentials.
  program-audio.json    <- scripts/program_audio_sampler.py (a --user service): the verdict on
                           whether the stream program audio is measurement-only (the YouTube
                           channel guard, `scripts/program_audio_guard.py`).

The serve dir is NEVER the lease dir: the lease dir's mere existence means `held=true`
(`scripts/rig_lease_state.py`), so a file written there would fake a held lease. Its default is
`/var/tmp/rig-lease-serve` (`$RIG_LEASE_SERVE_DIR` overrides it, for the server, the mirror and
the sampler alike). /var/tmp because the lease server unit runs with ProtectHome=read-only and
PrivateTmp=no (see systemd/rig-lease-server.service).

This module is stdlib-only on purpose: the lease server is a coordination endpoint and must not
gain a numpy dependency (the sampler's analysis lives in `program_audio.py`).
"""
from __future__ import annotations

import json
import os
import tempfile
from datetime import datetime, timezone

DEFAULT_SERVE_DIR = "/var/tmp/rig-lease-serve"
SERVE_DIR_ENV = "RIG_LEASE_SERVE_DIR"
MARKERS_NAME = "rig-qpsk-markers.csv"
PROGRAM_AUDIO_NAME = "program-audio.json"


DEFAULT_LEASE_DIR = "/var/tmp/rig-lease"  # scripts/lib/rig-lease.sh's own default ($RIG_LEASE_DIR)


def default_serve_dir() -> str:
    return os.environ.get(SERVE_DIR_ENV) or DEFAULT_SERVE_DIR


def default_lease_dir() -> str:
    return os.environ.get("RIG_LEASE_DIR") or DEFAULT_LEASE_DIR


def serve_dir_conflict(serve_dir: str, lease_dir: str) -> str | None:
    """A reason string when `serve_dir` is the lease dir or lies inside it (a writer's `mkdir -p`
    there would create the lease dir = a fake held lease), else None."""
    serve = os.path.realpath(serve_dir)
    lease = os.path.realpath(lease_dir)
    if serve == lease or serve.startswith(lease + os.sep):
        return f"serve dir {serve_dir} is (inside) the lease dir {lease_dir} -- its existence means held=true"
    return None


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
    """The mirrored file's bytes + its age in whole seconds (now - mtime, the moment the last
    successful mirror pass renamed it in), or None while the file is absent. Size and mtime come
    from the SAME open file (fstat), so a concurrent rename cannot pair one file's bytes with the
    other's age."""
    try:
        fh = open(path, "rb")
    except FileNotFoundError:
        return None  # absent: the server answers 404 (the documented contract)
    with fh:
        st = os.fstat(fh.fileno())
        data = fh.read()
    return data, max(0, int(now_epoch - st.st_mtime))


def program_audio_response(path: str, now: datetime) -> dict | None:
    """The program-audio payload as served: `age_s` recomputed at request time from the payload's
    own `ts_utc` (one clock, dev1's, so a consumer on another host never compares two clocks, and a
    sampler that stopped writing reads stale, never fresh). None while the file is absent. An
    unreadable file is served FAIL-CLOSED as UNKNOWN with `age_s` null; an unparseable `ts_utc`
    leaves the verdict but sets `age_s` null (a consumer treats a null age as stale)."""
    try:
        with open(path, "rb") as fh:
            raw = fh.read()
    except FileNotFoundError:
        return None  # absent: the server answers 404 (the documented contract)
    try:
        payload = json.loads(raw)
        if not isinstance(payload, dict):
            raise ValueError("not a JSON object")
    except ValueError as exc:
        return {
            "schema": 1, "ts_utc": None, "age_s": None, "verdict": "UNKNOWN", "rms_dbfs": None,
            "outside_band_pct": None, "window_s": None,
            "reason": f"program-audio.json unreadable: {exc}",
        }
    ts = parse_ts_utc(payload.get("ts_utc"))
    payload["age_s"] = None if ts is None else round((now - ts).total_seconds(), 1)
    return payload
