#!/usr/bin/env python3
"""issue 1450 -- the rig lease's named resources: the ONE parser of scripts/rig-lease-resources.txt.

A lease holder declares the boxes it touches (atoms, or groups of atoms) and a reader asks only for the
ones it needs, so a SongPlayer deploy on win-resolume no longer waits for a camera-box soak on the SNV
cameras. Every user of the table goes through this module, so bash and python can never read it
differently:
  - scripts/lib/rig-lease.sh `rig_lease_resources_resolve` runs `rig_lease_resources.py resolve <csv>`;
  - scripts/rig_lease_state.py (the :8890 view) and scripts/rig_lease_write.py (the POST acquire) import it.

Table format (one entry per line, `#` comments and blank lines ignored):
  atom|<name>|<what it covers>          -- the canonical atom order is the order of these lines
  group|<name>|<members, space-separated>  -- atoms, or groups defined ABOVE it
`all` is reserved and means every atom. A name is lower-case ASCII letters and digits in dash-separated
words. Anything else is a TableError naming the line: a broken table never resolves to "nothing".

CLI: `rig_lease_resources.py resolve <csv>` prints the resolved atoms as CSV (canonical order, each once)
and exits 0; a blank declaration prints nothing (undeclared = the legacy whole-rig holder); an unknown
name prints the unknown names as CSV and exits 2; an unreadable table exits 3 with the reason on stderr.
`RIG_LEASE_RESOURCES_TABLE` overrides the table path (tests).
"""
from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass, field

TABLE_ENV = "RIG_LEASE_RESOURCES_TABLE"
DEFAULT_TABLE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "rig-lease-resources.txt")
ALL = "all"

RC_OK = 0
RC_UNKNOWN = 2
RC_TABLE = 3

_NAME = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*", re.ASCII)
_PRINTABLE = re.compile(r"[^A-Za-z0-9._-]", re.ASCII)


class TableError(ValueError):
    """The resource table cannot be read or is malformed."""


class UnknownResources(ValueError):
    """A declaration names something the table does not know."""

    def __init__(self, names):
        super().__init__("unknown rig resource(s): " + ", ".join(names))
        self.names = list(names)


@dataclass(frozen=True)
class Table:
    atoms: tuple
    groups: dict = field(default_factory=dict)  # name -> tuple of atoms (canonical order), incl. ALL
    descriptions: dict = field(default_factory=dict)


def table_path() -> str:
    return os.environ.get(TABLE_ENV) or DEFAULT_TABLE


def _canonical(members: set, atoms: tuple) -> tuple:
    return tuple(a for a in atoms if a in members)


def parse_table(text: str, where: str = "the rig resource table") -> Table:
    atoms: list = []
    groups: dict = {}
    descriptions: dict = {}
    for n, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("|")
        if len(parts) != 3 or parts[0] not in ("atom", "group"):
            raise TableError(f"{where} line {n}: expected 'atom|<name>|<text>' or 'group|<name>|<members>'")
        kind, name, rest = parts[0], parts[1].strip(), parts[2].strip()
        if not _NAME.fullmatch(name):
            raise TableError(f"{where} line {n}: {name!r} is not a resource name")
        if name == ALL:
            raise TableError(f"{where} line {n}: 'all' is reserved (every atom)")
        if name in descriptions or name in groups:
            raise TableError(f"{where} line {n}: {name!r} is declared twice")
        if kind == "atom":
            if groups:
                raise TableError(f"{where} line {n}: atom {name!r} after the first group")
            atoms.append(name)
            descriptions[name] = rest
            continue
        members = rest.split()
        if not members:
            raise TableError(f"{where} line {n}: group {name!r} has no members")
        resolved: set = set()
        for m in members:
            if m in atoms:
                resolved.add(m)
            elif m in groups:
                resolved.update(groups[m])
            else:
                raise TableError(f"{where} line {n}: group {name!r} names {m!r}, which is no atom and no "
                                 "group defined above it")
        groups[name] = _canonical(resolved, tuple(atoms))
        descriptions[name] = rest
    if not atoms:
        raise TableError(f"{where}: no atom is declared")
    groups[ALL] = tuple(atoms)
    return Table(tuple(atoms), groups, descriptions)


def load_table(path: str | None = None) -> Table:
    path = path or table_path()
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
    except (OSError, UnicodeDecodeError) as exc:
        raise TableError(f"cannot read the rig resource table {path}: {exc}") from exc
    return parse_table(text, path)


def split_names(value: str) -> list:
    """A CSV declaration -> its names, whitespace stripped, empty items dropped."""
    return [p.strip() for p in str(value).split(",") if p.strip()]


def resolve(names, table: Table) -> list:
    """Names (atoms / groups / `all`) -> the atoms they cover, canonical order, each once.
    Raises UnknownResources naming every name the table does not know."""
    unknown = [n for n in names if n not in table.descriptions and n not in table.groups]
    if unknown:
        raise UnknownResources(unknown)
    covered: set = set()
    for n in names:
        covered.update(table.groups.get(n, (n,)))
    return list(_canonical(covered, table.atoms))


def printable(names) -> str:
    """Names for a log / a KEY=value line: never a newline or a shell metacharacter."""
    return ",".join(_PRINTABLE.sub("?", n) for n in names)


def main(argv) -> int:
    if len(argv) != 3 or argv[1] != "resolve":
        print("usage: rig_lease_resources.py resolve <csv of atoms/groups>", file=sys.stderr)
        return RC_TABLE
    names = split_names(argv[2])
    if not names:
        return RC_OK
    try:
        table = load_table()
        print(",".join(resolve(names, table)))
    except TableError as exc:
        print(f"rig_lease_resources: {exc}", file=sys.stderr)
        return RC_TABLE
    except UnknownResources as exc:
        print(printable(exc.names))
        return RC_UNKNOWN
    return RC_OK


if __name__ == "__main__":
    sys.exit(main(sys.argv))
