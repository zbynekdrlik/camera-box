#!/usr/bin/env python3
"""Issue 1372 part B -- python twin of scripts/lib/dantesync-fleet.sh + the config-drift decision.

Two jobs, both PURE (no network):

1. The fleet list for python consumers. Python cannot source the bash lib, so this reads the SAME
   `DANTESYNC_FLEET` table (the env var when set, exactly like the bash `${DANTESYNC_FLEET:-...}`
   default, else the default block parsed out of the lib), walks the SAME camera_resolve arms in
   scripts/camera-set.sh (cam1, cam2, ... to the first unknown name) and resolves `obs:<name>`
   addresses through scripts/obs_fleet_table.py. `rows()` prints byte-identical lines to the bash
   `dantesync_fleet_rows` (pinned by tests/python/test_dantesync_fleet_1372.py).

2. The config-drift decision. Each node's dantesync config.json is compared with ONE canonical
   template per role (scripts/dantesync-canonical-config.json): every template leaf must match, a
   node key the template lacks is an extra key, and a file that starts with a byte-order mark is
   drift on its own (dantesync then ignores the whole file and runs on defaults -- the 13.9.2026
   PowerShell-write incident). The verdict is OK / DRIFT / UNKNOWN (unreadable or not JSON), with a
   named diff line per difference. Report-only: nothing here writes a config anywhere.

CLI:
  dantesync_fleet.py rows
  dantesync_fleet.py gm-host ROLE
  dantesync_fleet.py template --role ROLE
  dantesync_fleet.py drift --role ROLE [--name NAME] [--templates PATH] CONFIG_FILE
Exit (drift): 0 = matches its template, 20 = DRIFT, 11 = UNKNOWN, 2 = usage.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import sys
from pathlib import Path

_SCRIPTS = Path(__file__).resolve().parent
FLEET_SH = _SCRIPTS / "lib" / "dantesync-fleet.sh"
CAMERA_SET_SH = _SCRIPTS / "camera-set.sh"
RIG_GM_SH = _SCRIPTS / "lib" / "rig-grandmaster.sh"
TEMPLATES_JSON = _SCRIPTS / "dantesync-canonical-config.json"
CAMERA_MAX = 99

ROLES = ("video", "audio", "ntp-master")
OK, DRIFT, UNKNOWN = "OK", "DRIFT", "UNKNOWN"
EXIT = {OK: 0, DRIFT: 20, UNKNOWN: 11}


def _load_obs_fleet_table():
    spec = importlib.util.spec_from_file_location("obs_fleet_table", _SCRIPTS / "obs_fleet_table.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _bash_default(var: str, path: Path) -> str:
    """The `${VAR:-default}` default of a `VAR="${VAR:-...}"` assignment in a bash file (may span
    lines). The env var wins, exactly like bash."""
    env = os.environ.get(var)
    if env:
        return env
    m = re.search(r'^' + re.escape(var) + r'="\$\{' + re.escape(var) + r':-(.*?)\}"', path.read_text(),
                  re.MULTILINE | re.DOTALL)
    if not m:
        raise RuntimeError(f"dantesync_fleet: no {var} default found in {path}")
    return m.group(1)


def camera_rows() -> list:
    """One row per camera camera_resolve knows, walked cam1, cam2, ... to the first unknown name."""
    arms = dict(re.findall(r"^\s*(cam[0-9]+)\)\s*CAMERA_IP=([0-9.]+);", CAMERA_SET_SH.read_text(),
                           re.MULTILINE))
    rows = []
    for n in range(1, CAMERA_MAX + 1):
        ip = arms.get(f"cam{n}")
        if ip is None:
            break
        rows.append(f"cam{n}|{ip}|linux|video|always|root|")
    return rows


def rows() -> list:
    """Every current dantesync node as `name|addr|os|role|homegate|user|credvar` (the bash
    dantesync_fleet_rows order). A malformed row or an unknown obs-fleet name raises."""
    obs = _load_obs_fleet_table()
    obs_rows = {r[0]: r for r in obs.fleet_rows()}
    out = camera_rows()
    for line in _bash_default("DANTESYNC_FLEET", FLEET_SH).splitlines():
        if not line.strip():
            continue
        parts = line.split("|")
        if len(parts) != 7 or not all(parts[:6]):
            raise RuntimeError(f"dantesync_fleet: malformed DANTESYNC_FLEET row {line!r}")
        name, addr, os_, role, homegate, user, credvar = parts
        if addr.startswith("obs:"):
            row = obs_rows.get(addr[4:])
            if row is None:
                raise RuntimeError(f"dantesync_fleet: row {name!r} names obs-fleet box {addr[4:]!r} absent from OBS_FLEET")
            if row[3] == "retired":
                continue
            addr = row[1]
        out.append("|".join([name, addr, os_, role, homegate, user, credvar]))
    return out


def role_gm_host(role: str) -> str:
    """The grandmaster HOST a node of ROLE must lock to (the bash dantesync_fleet_role_gm_host)."""
    if role in ("video", "ntp-master"):
        return os.environ.get("RIG_GRANDMASTER_HOST") or re.search(
            r'^RIG_GRANDMASTER_HOST_DEFAULT="([^"]+)"', RIG_GM_SH.read_text(), re.MULTILINE).group(1)
    if role == "audio":
        return _bash_default("DANTESYNC_AUDIO_GM_HOST", FLEET_SH)
    raise ValueError(f"unknown role {role!r} (expected one of {', '.join(ROLES)})")


# ---------------------------------------------------------------------------------------------
# config drift
# ---------------------------------------------------------------------------------------------

def load_templates(path: Path = TEMPLATES_JSON) -> dict:
    return json.loads(Path(path).read_text())["roles"]


def _resolve_tokens(value):
    if isinstance(value, str):
        if value == "@video_gm":
            return role_gm_host("video")
        if value == "@audio_gm":
            return role_gm_host("audio")
        return value
    if isinstance(value, list):
        return [_resolve_tokens(v) for v in value]
    if isinstance(value, dict):
        return {k: _resolve_tokens(v) for k, v in value.items()}
    return value


def template_for(role: str, templates: dict | None = None) -> dict:
    templates = load_templates() if templates is None else templates
    if role not in templates:
        raise ValueError(f"unknown role {role!r} (templates carry {', '.join(sorted(templates))})")
    return _resolve_tokens(templates[role])


def _is_rule(v) -> bool:
    return isinstance(v, dict) and len(v) == 1 and next(iter(v)) in ("$optional", "$any")


def _fmt(v) -> str:
    return json.dumps(v, sort_keys=True)


def _compare(tpl: dict, node: dict, prefix: str, diffs: list) -> None:
    for key, want in tpl.items():
        if key.startswith("_"):
            continue
        path = f"{prefix}{key}"
        if _is_rule(want):
            rule, arg = next(iter(want.items()))
            if key not in node:
                if rule == "$any":
                    diffs.append(f"{path}: missing (canonical: any value)")
                continue
            if rule == "$optional" and node[key] != arg:
                diffs.append(f"{path}: {_fmt(node[key])} (canonical {_fmt(arg)})")
            continue
        if key not in node:
            if isinstance(want, dict):
                # name every missing LEAF (e.g. system.phase_slew.enabled), not just the parent
                _compare(want, {}, path + ".", diffs)
            else:
                diffs.append(f"{path}: missing (canonical {_fmt(want)})")
            continue
        have = node[key]
        if isinstance(want, dict) and isinstance(have, dict):
            _compare(want, have, path + ".", diffs)
        elif have != want:
            diffs.append(f"{path}: {_fmt(have)} (canonical {_fmt(want)})")
    for key, have in node.items():
        if key.startswith("_") or key in tpl:
            continue
        diffs.append(f"{prefix}{key}: extra key {_fmt(have)} (not in the canonical template)")


def drift(raw: bytes | None, role: str, templates: dict | None = None) -> tuple:
    """(verdict, diffs) for one node's config.json bytes against its role template."""
    if not raw:
        return UNKNOWN, ["config.json could not be read (empty or unreachable)"]
    diffs = []
    if raw.startswith(b"\xef\xbb\xbf"):
        diffs.append("file starts with a UTF-8 byte-order mark: dantesync ignores the file and runs "
                     "on defaults (the PowerShell-write trap)")
        raw = raw[3:]
    elif raw.startswith((b"\xff\xfe", b"\xfe\xff")):
        return DRIFT, ["file is UTF-16 encoded (a PowerShell write): dantesync cannot read it and runs "
                       "on defaults"]
    try:
        node = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        return UNKNOWN, [f"config.json is not valid JSON: {exc}"]
    if not isinstance(node, dict):
        return UNKNOWN, ["config.json is not a JSON object"]
    _compare(template_for(role, templates), node, "", diffs)
    return (DRIFT if diffs else OK), diffs


def render(name: str, role: str, verdict: str, diffs: list) -> str:
    lines = [f"node={name} role={role} verdict={verdict} diffs={len(diffs) if verdict != UNKNOWN else 0}"]
    lines += [f"  - {d}" for d in diffs]
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(prog="dantesync_fleet.py", description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("rows", help="print every dantesync node row")
    g = sub.add_parser("gm-host", help="the grandmaster host a role must lock to")
    g.add_argument("role", choices=ROLES)
    t = sub.add_parser("template", help="print a role's canonical config (tokens resolved)")
    t.add_argument("--role", required=True, choices=ROLES)
    t.add_argument("--templates", default=str(TEMPLATES_JSON))
    d = sub.add_parser("drift", help="compare one node's config.json with its role template")
    d.add_argument("--role", required=True, choices=ROLES)
    d.add_argument("--name", default="node")
    d.add_argument("--templates", default=str(TEMPLATES_JSON))
    d.add_argument("config")
    ns = ap.parse_args(argv)
    if ns.cmd == "rows":
        print("\n".join(rows()))
        return 0
    if ns.cmd == "gm-host":
        print(role_gm_host(ns.role))
        return 0
    if ns.cmd == "template":
        print(json.dumps(template_for(ns.role, load_templates(ns.templates)), indent=2, sort_keys=True))
        return 0
    try:
        raw = Path(ns.config).read_bytes()
    except OSError:
        raw = None
    verdict, diffs = drift(raw, ns.role, load_templates(ns.templates))
    print(render(ns.name, ns.role, verdict, diffs))
    return EXIT[verdict]


if __name__ == "__main__":
    sys.exit(main())
