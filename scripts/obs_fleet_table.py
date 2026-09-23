"""Python reader of the ONE managed-OBS-box table in scripts/lib/obs-fleet.sh (issue 1317 part 4).

Python cannot source the bash lib, so this module reads the SAME `OBS_FLEET` table (the `OBS_FLEET`
env var when set, exactly like the bash `${OBS_FLEET:-...}` default, else the default block parsed
out of scripts/lib/obs-fleet.sh) and mirrors its alias-aware host lookup:

  fleet_name_for_host  == obs-fleet.sh `obs_fleet_name_for_host`
  fleet_class_for_host == obs-fleet.sh `obs_fleet_class_for_host`

so the python consumers (rig-health-audit.py's strih_platform twin, the phase/av-sync calibrate push
plans) decide "which box is this address" from the same rows the dev1 watchdogs use -- never a second
hand-kept host map. Parity is pinned by tests/python/test_strih_windows_remnants_1317.py, which runs
the bash functions side by side.
"""
from __future__ import annotations

import os
import re
import socket
from pathlib import Path
from typing import Callable

OBS_FLEET_SH = Path(__file__).resolve().parent / "lib" / "obs-fleet.sh"

_DEFAULT_RE = re.compile(r'^OBS_FLEET="\$\{OBS_FLEET:-(.*?)\}"', re.MULTILINE | re.DOTALL)
_IPV4_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$")


def _table_text() -> str:
    env = os.environ.get("OBS_FLEET")
    if env:
        return env
    m = _DEFAULT_RE.search(OBS_FLEET_SH.read_text())
    if not m:
        raise RuntimeError(f"obs_fleet_table: no OBS_FLEET default block found in {OBS_FLEET_SH}")
    return m.group(1)


def fleet_rows() -> list[tuple[str, str, str, str]]:
    """Every `name|host|class|home-check` row, in table order. Blank / whitespace-only lines are
    skipped (bash treats such a line as a row that can never match -- the same outcome); a
    MALFORMED row raises on purpose, so a broken table is a loud error, never a silent mis-route."""
    rows = []
    for line in _table_text().splitlines():
        if not line.strip():
            continue
        parts = line.split("|")
        if len(parts) != 4:
            raise RuntimeError(f"obs_fleet_table: malformed OBS_FLEET row {line!r}")
        rows.append((parts[0], parts[1], parts[2], parts[3]))
    return rows


def fleet_host(name: str) -> str | None:
    for row in fleet_rows():
        if row[0] == name:
            return row[1]
    return None


def fleet_class(name: str) -> str | None:
    for row in fleet_rows():
        if row[0] == name:
            return row[2]
    return None


def _resolve(host: str) -> str:
    """The FIRST resolved address (getent ahosts's first line), '' when unresolvable."""
    try:
        infos = socket.getaddrinfo(host, None)
    except (socket.gaierror, UnicodeError, OSError):
        return ""
    return infos[0][4][0] if infos else ""


def fleet_name_for_host(host: str, resolve: Callable[[str], str] | None = None) -> str | None:
    """The fleet NAME HOST addresses, or None. Same three steps as the bash lookup: exact name/host
    (case-folded), a DNS name's first label vs the row name (never for an IPv4 literal), then the
    resolved address vs the row host fields (an IPv4 literal is never resolved)."""
    if not host:
        return None
    rows = fleet_rows()
    for name, row_host, _cls, _check in rows:
        if name.lower() == host.lower() or row_host.lower() == host.lower():
            return name
    if _IPV4_RE.match(host):
        return None
    if "." in host:
        short = host.split(".", 1)[0].lower()
        for name, _row_host, _cls, _check in rows:
            if name.lower() == short:
                return name
    ip = (resolve or _resolve)(host)
    if not ip:
        return None
    for name, row_host, _cls, _check in rows:
        if row_host == ip:
            return name
    return None


def fleet_class_for_host(host: str, resolve: Callable[[str], str] | None = None) -> str | None:
    """windows-genlock | linux-genlock for the row HOST addresses, or None when no row does."""
    name = fleet_name_for_host(host, resolve=resolve)
    return fleet_class(name) if name else None
