#!/usr/bin/env python3
"""#1277 -- PURE decision mirror of scripts/lib/rig-lease.sh's staleness/read logic, for the
read-only HTTP exposure of the #830 cross-repo rig lease (scripts/rig-lease-server.py).

WHY (see the issue's own design comment for the full root cause / rejected alternatives): #830's
lockdir contract assumes both lease participants run ON dev1. That is false for restreamer's
Windows stream-box runner (camera-box#1277 / restreamer#349) -- it can only reach dev1 over LAN
HTTP, not the local filesystem. This module reads the SAME lockdir
(`/var/tmp/rig-lease/holder.json` + `/var/tmp/rig-lease/heartbeat`, written by
`scripts/lib/rig-lease.sh::rig_lease_write_holder`) and reproduces its staleness verdict as a pure,
side-effect-free function of (lease_dir, now, stale_secs) -- exactly the "pure kernel read by a thin
transport wrapper" split this repo already uses for bundle_state_gather.py/bundle-state-server.py.

Mirrors, field-for-field:
  - rig_lease_heartbeat_age_seconds  -> a MISSING heartbeat file reads as the same huge sentinel age
                                        (999999999) the bash function uses -- never mistaken for fresh.
  - rig_lease_is_fresh               -> age must be >=0 AND strictly < stale_secs to count as fresh.
  - rig_lease_is_stale (WITHOUT the pluggable RIG_LEASE_RUN_STATUS_CMD leg): an ABSENT holder.json
    (the lockdir exists but the file inside it does not -- e.g. a crashed acquire) is UNCONDITIONALLY
    stale/reclaimable, exactly like `[ -f "$path" ] || return 0`. A PRESENT-but-unparseable
    holder.json is NOT forced stale -- staleness still rests on the heartbeat alone, exactly like the
    bash function (which only checks file EXISTENCE, never validity, before falling through to the
    heartbeat check). The RIG_LEASE_RUN_STATUS_CMD leg is deliberately NOT mirrored here: it is unset
    in every real deployment (the bash default is always "in_progress", a no-op), and giving a
    read-only stdlib HTTP server the ability to shell out to an external status-check command would be
    a needless write-adjacent capability for a read-only service to carry.

Fail-closed by construction (the issue's own contract): a missing/absent/corrupt holder.json NEVER
resolves to "held=False" -- the lockdir's mere EXISTENCE is "held=true, holder=null" at worst, never
"nothing is held". Only a genuinely ABSENT lockdir (`os.path.isdir(lease_dir)` False) is held=False,
in which case every OTHER field is also None (a consumer only ever needs `stale` when `held` is true).
"""
from __future__ import annotations

import json
import os
from datetime import datetime, timezone
from typing import Optional

# Matches scripts/lib/rig-lease.sh::rig_lease_heartbeat_age_seconds's sentinel exactly (a missing
# heartbeat file prints `999999999`) -- a huge, never-fresh age, never null.
HEARTBEAT_MISSING_SENTINEL = 999999999

# The exact timestamp format scripts/lib/rig-lease.sh::rig_lease_write_holder writes via
# `date -u +%Y-%m-%dT%H:%M:%SZ` -- used for both holder.json's own timestamps and this module's
# own `now` field, so a consumer never has to handle two different date shapes from one lease.
_TS_FORMAT = "%Y-%m-%dT%H:%M:%SZ"

# The exact fields scripts/lib/rig-lease.sh::rig_lease_write_holder writes into holder.json.
_HOLDER_FIELDS = ("repo", "run_id", "run_url", "job", "acquired_at", "expected_release_at")

SCHEMA_VERSION = 1

# scripts/rig-busy-gate.sh:79 -- RIG_LEASE_STALE_SECS="${RIG_LEASE_STALE_SECS:-5400}". Kept as a
# named constant (not a bare literal) so the lock-step test can assert equality without duplicating
# the number a third time; see test_rig_lease_state_1277.py::test_default_stale_secs_matches_rig_busy_gate_sh.
DEFAULT_STALE_SECS = 5400


def format_ts(dt: datetime) -> str:
    """Render a datetime as the same Z-suffixed UTC string rig-lease.sh writes/expects."""
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.astimezone(timezone.utc).strftime(_TS_FORMAT)


def parse_ts(text: Optional[str]) -> Optional[datetime]:
    """Parse a rig-lease.sh timestamp string. None on absence or any malformed value -- never raises,
    since holder.json content is foreign-written data this module must never crash on."""
    if not text:
        return None
    try:
        return datetime.strptime(text, _TS_FORMAT).replace(tzinfo=timezone.utc)
    except (ValueError, TypeError):
        return None


def _aware(now: datetime) -> datetime:
    return now if now.tzinfo is not None else now.replace(tzinfo=timezone.utc)


def _heartbeat_age_seconds(lease_dir: str, now: datetime) -> int:
    """Mirror rig_lease_heartbeat_age_seconds: a missing/unreadable heartbeat file is the sentinel."""
    hb_path = os.path.join(lease_dir, "heartbeat")
    try:
        mtime = os.stat(hb_path).st_mtime
    except OSError:
        return HEARTBEAT_MISSING_SENTINEL
    return int(_aware(now).timestamp() - mtime)


def _is_fresh(age_s: int, stale_secs: int) -> bool:
    """Mirror rig_lease_is_fresh: non-negative AND strictly under the threshold."""
    return age_s >= 0 and age_s < stale_secs


def _read_holder(lease_dir: str):
    """Return (exists, parsed_dict_or_None).

    exists=False -> holder.json is genuinely ABSENT (forces stale=True upstream, mirroring
                    `[ -f "$path" ] || return 0`).
    exists=True, dict=None -> present but unparseable/not-an-object (staleness stays
                    heartbeat-driven, mirroring the bash function's own existence-only check).
    """
    path = os.path.join(lease_dir, "holder.json")
    if not os.path.isfile(path):
        return False, None
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
        if not isinstance(data, dict):
            return True, None
        return True, data
    except (OSError, ValueError):
        return True, None


def lease_state(lease_dir: str, now: datetime, stale_secs: int = DEFAULT_STALE_SECS) -> dict:
    """The single pure decision this module exists for -- see the module doc for the full mirror
    contract. Never raises on a missing/corrupt lease dir; every failure degrades to the documented
    fail-closed shape (held=true, holder=null) rather than a crash or a false "not held"."""
    result = {
        "schema": SCHEMA_VERSION,
        "now": format_ts(now),
        "held": False,
        "holder": None,
        "heartbeat_age_s": None,
        "stale": None,
        "expected_release_at": None,
        "ttl_s": None,
    }

    if not os.path.isdir(lease_dir):
        return result

    result["held"] = True
    age = _heartbeat_age_seconds(lease_dir, now)
    result["heartbeat_age_s"] = age

    holder_exists, holder_data = _read_holder(lease_dir)

    if not holder_exists:
        # Mirrors `[ -f "$path" ] || return 0` -- an absent holder.json is unconditionally
        # stale/reclaimable, independent of the heartbeat (a crashed acquire leaves exactly this).
        result["stale"] = True
        return result

    result["stale"] = not _is_fresh(age, stale_secs)

    if holder_data is None:
        # Present but unparseable -- fail-closed (held=true) but nothing more can be reported.
        return result

    result["holder"] = {field: holder_data.get(field) for field in _HOLDER_FIELDS}
    expected = holder_data.get("expected_release_at")
    result["expected_release_at"] = expected

    expected_dt = parse_ts(expected)
    if expected_dt is not None:
        result["ttl_s"] = int((expected_dt - _aware(now)).total_seconds())

    return result
