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

import rig_lease_resources as rlr

# Matches scripts/lib/rig-lease.sh::rig_lease_heartbeat_age_seconds's sentinel exactly (a missing
# heartbeat file prints `999999999`) -- a huge, never-fresh age, never null.
HEARTBEAT_MISSING_SENTINEL = 999999999

# The exact timestamp format scripts/lib/rig-lease.sh::rig_lease_write_holder writes via
# `date -u +%Y-%m-%dT%H:%M:%SZ` -- used for both holder.json's own timestamps and this module's
# own `now` field, so a consumer never has to handle two different date shapes from one lease.
_TS_FORMAT = "%Y-%m-%dT%H:%M:%SZ"

# The exact fields scripts/lib/rig-lease.sh::rig_lease_write_holder writes into holder.json.
_HOLDER_FIELDS = ("repo", "run_id", "run_url", "job", "acquired_at", "expected_release_at")

# NEVER bump this (issue 1450): restreamer's reader treats `schema != 1` as an unknown format and
# PROCEEDS (scripts/ci/rig-lease-wait.ps1:88, Get-RigLeaseHolder in obs-ws.ps1:160), so a bump would
# silently switch its guard off. The box-scoped view is APPENDED after the v1 keys instead and
# announced by LEASE_MODEL.
SCHEMA_VERSION = 1
LEASE_MODEL = 2

# issue 1450: the v1 keys, in their served order -- every view starts with exactly these.
V1_KEYS = ("schema", "now", "held", "holder", "heartbeat_age_s", "stale", "expected_release_at", "ttl_s")

# scripts/rig-busy-gate.sh:79 -- RIG_LEASE_STALE_SECS="${RIG_LEASE_STALE_SECS:-5400}". Kept as a
# named constant (not a bare literal) so the lock-step test can assert equality without duplicating
# the number a third time; see test_rig_lease_state_1277.py::test_default_stale_secs_matches_rig_busy_gate_sh.
DEFAULT_STALE_SECS = 5400

# issue 1432: a lockdir with no holder.json is an acquire in progress (mkdir done, holder.json not
# yet written) for a few ms, and a crashed acquire's leftover after that. It is reclaimable only once
# the lockdir is this many seconds old -- mirrors RIG_LEASE_NOHOLDER_GRACE_SECS in
# scripts/lib/rig-lease.sh (pinned equal by test_rig_lease_write_1432.py).
NO_HOLDER_GRACE_SECS = 10


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
    except FileNotFoundError:
        # A concurrent release (scripts/lib/rig-lease.sh's #857 atomic rename-aside teardown) can
        # remove the file in the TOCTOU window between the isfile() check above and this open() --
        # treat this exactly like "absent" (forces stale=True upstream), never "corrupt" (which
        # stays heartbeat-driven and could otherwise report stale=False for a lease already gone).
        return False, None
    except (OSError, ValueError):
        return True, None


def holder_stale_secs(holder_data, default: int) -> int:
    """issue 1432: a holder may declare its OWN heartbeat-staleness threshold (`stale_secs`, a
    positive int in holder.json; the HTTP write routes write 180 for a remote holder that beats
    every 30 s). Mirrors scripts/lib/rig-lease.sh::rig_lease_is_stale: anything else (absent, a
    string, a bool, <= 0, an unparseable holder) falls back to the caller's threshold."""
    if isinstance(holder_data, dict):
        value = holder_data.get("stale_secs")
        if isinstance(value, int) and not isinstance(value, bool) and value > 0:
            return value
    return default


def _no_holder_grace_secs() -> int:
    """NO_HOLDER_GRACE_SECS, or the RIG_LEASE_NOHOLDER_GRACE_SECS override bash honours too."""
    raw = os.environ.get("RIG_LEASE_NOHOLDER_GRACE_SECS", "")
    return int(raw) if raw.isascii() and raw.isdigit() else NO_HOLDER_GRACE_SECS


def _no_holder_grace_expired(lease_dir: str, now: datetime) -> bool:
    """True when a holderless lockdir is older than NO_HOLDER_GRACE_SECS (or its age cannot be
    read: a crashed acquire must stay reclaimable)."""
    try:
        mtime = os.stat(lease_dir).st_mtime
    except OSError:
        return True
    return (_aware(now).timestamp() - mtime) >= _no_holder_grace_secs()


def fail_closed_state(now: datetime) -> dict:
    """The shape scripts/rig-lease-server.py serves when lease_state() itself raises (should never
    happen -- it is pure and catches its own I/O errors -- but a request handler must NEVER 500 on
    bad lease contents). Kept here, not duplicated in the server, so both the schema this function
    returns and lease_state()'s own schema have exactly ONE source of truth."""
    return {
        "schema": SCHEMA_VERSION,
        "now": format_ts(now),
        "held": True,
        "holder": None,
        "heartbeat_age_s": None,
        "stale": None,
        "expected_release_at": None,
        "ttl_s": None,
    }


def lease_state(lease_dir: str, now: datetime, stale_secs: int = DEFAULT_STALE_SECS) -> dict:
    """The single pure decision this module exists for -- see the module doc for the full mirror
    contract. Never raises on a missing/corrupt lease dir; every failure degrades to the documented
    fail-closed shape (held=true, holder=null) rather than a crash or a false "not held"."""
    return _dir_state(lease_dir, now, stale_secs)[0]


def _dir_state(lease_dir: str, now: datetime, stale_secs: int):
    """lease_state's decision + the holder.json it was made from (None when absent / unparseable):
    issue 1450's view reads the declared resources from the SAME parse, so a release between two
    reads can never pair one holder's state with another's declaration."""
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
        return result, None

    result["held"] = True
    age = _heartbeat_age_seconds(lease_dir, now)
    result["heartbeat_age_s"] = age

    holder_exists, holder_data = _read_holder(lease_dir)

    if not holder_exists and not _no_holder_grace_expired(lease_dir, now):
        # issue 1432: an acquire in progress -- held, holder not written yet, never reclaimable.
        result["stale"] = False
        return result, None

    if not holder_exists:
        # Mirrors `[ -f "$path" ] || return 0` -- an absent holder.json is unconditionally
        # stale/reclaimable, independent of the heartbeat (a crashed acquire leaves exactly this).
        result["stale"] = True
        return result, None

    result["stale"] = not _is_fresh(age, holder_stale_secs(holder_data, stale_secs))

    if holder_data is None:
        # Present but unparseable -- fail-closed (held=true) but nothing more can be reported.
        return result, None

    result["holder"] = {field: holder_data.get(field) for field in _HOLDER_FIELDS}
    expected = holder_data.get("expected_release_at")
    result["expected_release_at"] = expected

    expected_dt = parse_ts(expected)
    if expected_dt is not None:
        result["ttl_s"] = int((expected_dt - _aware(now)).total_seconds())

    return result, holder_data


# ---------------------------------------------------------------------------------------------------
# issue 1450 -- the box-scoped view. A holder declares the rig resources it touches (holder.json
# `resources`, the atoms of scripts/rig-lease-resources.txt, written by rig_lease_acquire from
# RIG_LEASE_RESOURCES); a holder.json without them -- every holder before issue 1450, every holder that
# declares nothing, an unparseable one, an acquire in progress -- covers ALL atoms (fail-closed). The
# v1 keys stay first and unchanged (SCHEMA_VERSION above); lease_model, resources and groups follow.
# Slice A keeps ONE lockdir, so at most one holder covers any atom.
# ---------------------------------------------------------------------------------------------------

def _free_entry() -> dict:
    return {"held": False, "via": None, "holder": None, "declared": None, "heartbeat_age_s": None,
            "stale": None, "expected_release_at": None, "ttl_s": None}


def _coverage(v1: dict, holder_data, table) -> tuple:
    """-> (the atoms the current holder covers, "legacy" | "scoped" | None, its declared atoms)."""
    if not v1["held"]:
        return set(), None, None
    declared = holder_data.get("resources") if isinstance(holder_data, dict) else None
    if isinstance(declared, list) and declared and all(isinstance(n, str) for n in declared):
        try:
            atoms = rlr.resolve(declared, table)
        except rlr.UnknownResources:
            atoms = None  # a declaration this table cannot read covers everything
        if atoms:
            return set(atoms), "scoped", atoms
    return set(table.atoms), "legacy", None


def _entry(atom: str, v1: dict, covered: set, via, declared) -> dict:
    if atom not in covered:
        return _free_entry()
    return {"held": True, "via": via, "holder": v1["holder"], "declared": declared,
            "heartbeat_age_s": v1["heartbeat_age_s"], "stale": v1["stale"],
            "expected_release_at": v1["expected_release_at"], "ttl_s": v1["ttl_s"]}


def _not_held(now: datetime) -> dict:
    return {"schema": SCHEMA_VERSION, "now": format_ts(now), "held": False, "holder": None,
            "heartbeat_age_s": None, "stale": None, "expected_release_at": None, "ttl_s": None}


def _scope_atoms(scope: str, table):
    """A `?resources=` value -> (its atoms, None) or (None, the reason it is refused)."""
    names = rlr.split_names(scope)
    if not names:
        return None, "empty resources= scope -- answering for the whole rig"
    try:
        return rlr.resolve(names, table), None
    except rlr.UnknownResources as exc:
        return None, (f"unknown rig resource(s) {rlr.printable(exc.names)} in the resources= scope "
                      "(scripts/rig-lease-resources.txt) -- answering for the whole rig")


def lease_view(lease_dir: str, now: datetime, stale_secs: int = DEFAULT_STALE_SECS,
               scope: Optional[str] = None, table=None) -> dict:
    """The served GET /rig-lease.json document. scope None = the whole rig. A scope (the
    `?resources=` CSV of atoms / groups) answers the v1 keys for those atoms only: held iff the holder
    covers any of them, else the not-held shape. An unknown or empty scope answers the WHOLE rig +
    `scope_error` (fail-closed: a 400 would make a consumer fail open). Raises only when the resource
    table itself is unreadable; the server then serves fail_closed_view()."""
    table = table if table is not None else rlr.load_table()
    v1, holder_data = _dir_state(lease_dir, now, stale_secs)
    covered, via, declared = _coverage(v1, holder_data, table)
    atoms, scope_error = (list(table.atoms), None) if scope is None else _scope_atoms(scope, table)
    if atoms is None:
        atoms = list(table.atoms)
    doc = dict(v1)
    if scope is not None and scope_error is None and not covered.intersection(atoms):
        doc = _not_held(now)
    doc["lease_model"] = LEASE_MODEL
    if scope is not None:
        if scope_error is None:
            doc["scope"] = atoms
        else:
            doc["scope_error"] = scope_error
    doc["resources"] = {a: _entry(a, v1, covered, via, declared) for a in atoms}
    doc["groups"] = {g: list(m) for g, m in table.groups.items()}
    return doc


def fail_closed_view(now: datetime, scope: Optional[str] = None) -> dict:
    """What the server serves when lease_view() raises (an unreadable resource table): the v1
    fail-closed shape for the whole rig, every scope included, plus the model marker."""
    doc = fail_closed_state(now)
    doc["lease_model"] = LEASE_MODEL
    if scope is not None:
        doc["scope_error"] = "the rig lease view could not be computed -- answering held for the whole rig"
    return doc
