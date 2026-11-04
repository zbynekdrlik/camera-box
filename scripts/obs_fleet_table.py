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
import subprocess
from pathlib import Path
from typing import Callable

OBS_FLEET_SH = Path(__file__).resolve().parent / "lib" / "obs-fleet.sh"

# issue 1414: the transition aliases, the python twin of obs-fleet.sh OBS_FLEET_ALIASES (pinned equal by
# tests/python/test_box_names_venue_1414.py): the old name of a renamed SNV row -> the row.
FLEET_ALIASES = {"strih-lx": "strih-snv", "stream": "stream-snv", "resolume": "resolume-snv"}  # issue 1414

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


def _fleet_row(name: str) -> tuple[str, str, str, str] | None:
    """The row NAME addresses: an exact row wins, else a transition alias whose renamed row is in the
    table (issue 1414, the bash `_obs_fleet_entry`)."""
    rows = fleet_rows()
    for row in rows:
        if row[0] == name:
            return row
    target = FLEET_ALIASES.get(name.lower())
    for row in rows:
        if target is not None and row[0] == target:
            return row
    return None


def fleet_host(name: str) -> str | None:
    row = _fleet_row(name)
    return row[1] if row else None


def fleet_class(name: str) -> str | None:
    row = _fleet_row(name)
    return row[2] if row else None


def _resolve(host: str) -> str:
    """The FIRST resolved address (getent ahosts's first line), '' when unresolvable."""
    try:
        infos = socket.getaddrinfo(host, None)
    except (socket.gaierror, UnicodeError, OSError):
        return ""
    return infos[0][4][0] if infos else ""


def fleet_name_for_host(host: str, resolve: Callable[[str], str] | None = None) -> str | None:
    """The fleet NAME HOST addresses, or None. Same steps as the bash lookup: exact name/host
    (case-folded), an issue-1414 transition alias, a DNS name's first label vs the row name (then vs
    the aliases; never for an IPv4 literal), then the resolved address vs the row host fields (an
    IPv4 literal is never resolved)."""
    if not host:
        return None
    rows = fleet_rows()
    for name, row_host, _cls, _check in rows:
        if name.lower() == host.lower() or row_host.lower() == host.lower():
            return name
    if _IPV4_RE.match(host):
        return None
    names = {r[0] for r in rows}
    if FLEET_ALIASES.get(host.lower()) in names:
        return FLEET_ALIASES[host.lower()]
    if "." in host:
        short = host.split(".", 1)[0].lower()
        for name, _row_host, _cls, _check in rows:
            if name.lower() == short:
                return name
        if FLEET_ALIASES.get(short) in names:
            return FLEET_ALIASES[short]
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


# ------------------------------------------------------------------ issue 1404: the home check
# The python twin of obs-fleet.sh `obs_fleet_canonical_name`, `obs_fleet_home_check`,
# `obs_fleet_expected_identity` and `obs_fleet_is_home`, for the strih-snv program-audio sampler's CG
# arm (scripts/program_audio_cg.py), which cannot source the bash lib. Parity is pinned by
# tests/python/test_program_audio_cg_fleet_1404.py, which runs both sides.

_IDENTITY_RE = re.compile(r'^OBS_FLEET_IDENTITY="\$\{OBS_FLEET_IDENTITY-(.*?)\}"', re.MULTILINE)
DEFAULT_HOME_PORT = 4455      # obs-fleet.sh OBS_FLEET_HOME_PORT default: the OBS-WS port
DEFAULT_STATUS_TIMEOUT_S = 4  # obs-fleet.sh OBS_FLEET_STATUS_TIMEOUT default


def fleet_canonical_name(name: str) -> str:
    """The row NAME addresses: an exact row, else its transition alias, else the case-folded row, else
    NAME itself (obs_fleet_canonical_name)."""
    names = [r[0] for r in fleet_rows()]
    if name in names:
        return name
    alias = FLEET_ALIASES.get(name.lower())
    if alias in names:
        return alias
    if name.lower() in names:
        return name.lower()
    return name


def fleet_home_check(name: str) -> str | None:
    """always | traveling | retired for NAME's row, or None when no row addresses it."""
    row = _fleet_row(name)
    return row[3] if row else None


def _identity_text() -> str:
    # bash `${OBS_FLEET_IDENTITY-...}`: a SET variable wins even when empty
    if "OBS_FLEET_IDENTITY" in os.environ:
        return os.environ["OBS_FLEET_IDENTITY"]
    m = _IDENTITY_RE.search(OBS_FLEET_SH.read_text())
    if not m:
        raise RuntimeError(f"obs_fleet_table: no OBS_FLEET_IDENTITY default found in {OBS_FLEET_SH}")
    return m.group(1)


def fleet_expected_identity(name: str) -> str | None:
    """The declared computer name of NAME's row (OBS_FLEET_IDENTITY), None when none is declared."""
    want = fleet_canonical_name(name)
    for pair in _identity_text().split():
        key, _, value = pair.partition("=")
        if key == want:
            return value or None
    return None


def _status_probe(host: str, port: int, timeout_s: float) -> bool:
    """True when a TCP connect to host:port succeeds within timeout_s (obs_fleet_status_probe)."""
    try:
        with socket.create_connection((host, port), timeout=timeout_s):
            return True
    except OSError:
        return False


def fleet_is_home(name: str, resolve: Callable[[str], str] | None = None,
                  probe: Callable[[str, int], bool] | None = None) -> bool:
    """obs_fleet_is_home: the OBS_FLEET_HOME force-list (space-separated names, aliases matched) is
    authoritative when set; else `always` is home, `retired` never, `traveling` only when its host
    resolves AND its OBS-WS port (OBS_FLEET_HOME_PORT, 4455) answers; an unknown name is not home."""
    forced = os.environ.get("OBS_FLEET_HOME")
    if forced:
        want = fleet_canonical_name(name)
        return any(fleet_canonical_name(w) == want for w in forced.split())
    check = fleet_home_check(name)
    if check == "always":
        return True
    if check != "traveling":
        return False
    host = fleet_host(name)
    if resolve is None and os.environ.get("OBS_FLEET_RESOLVE_CMD"):
        resolve = _seam_resolve
    if not host or not (resolve or _resolve)(host):
        return False
    port = int(os.environ.get("OBS_FLEET_HOME_PORT") or DEFAULT_HOME_PORT)
    if probe is not None:
        return bool(probe(host, port))
    if os.environ.get("OBS_FLEET_STATUS_PROBE_CMD"):
        return _seam_probe(host, port)
    timeout_s = float(os.environ.get("OBS_FLEET_STATUS_TIMEOUT") or DEFAULT_STATUS_TIMEOUT_S)
    return _status_probe(host, port, timeout_s)


def _seam_resolve(host: str) -> str:
    """obs-fleet.sh's OBS_FLEET_RESOLVE_CMD seam (`<cmd> <host>` -> its first output line, '' when
    it prints nothing or cannot run: not resolvable, like the bash lib)."""
    try:
        r = subprocess.run([os.environ["OBS_FLEET_RESOLVE_CMD"], host], capture_output=True, text=True,
                           timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        return ""
    lines = r.stdout.splitlines()
    return lines[0].strip() if lines else ""


def _seam_probe(host: str, port: int) -> bool:
    """obs-fleet.sh's OBS_FLEET_STATUS_PROBE_CMD seam (`<cmd> <host> <port>`, exit 0 = answers)."""
    try:
        return subprocess.run([os.environ["OBS_FLEET_STATUS_PROBE_CMD"], host, str(port)],
                              capture_output=True, timeout=10).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False
