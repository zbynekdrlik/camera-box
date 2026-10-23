#!/usr/bin/env python3
"""issue 1439 follow-up: the bridge VLAN / bridge port / QoS profile / switch rule facet of the
venue netcfg audit (scripts/netcfg-audit.sh, .claude/rules/netcfg-audit.md).

The VBAN-only legs need switch config that is otherwise recorded nowhere (23.10.2026): foh2_video
`sfp-sfpplus2` VLAN 89 untagged via PVID + VLAN 70 tagged with `frame-types=admit-all`, the QoS
profile `dante-audio` (DSCP 46 -> traffic class 5) and its `new-qos-profile` switch rule. This
module parses one read-only `print detail` pass per CRS310 (GATHER_CMD), turns it into the
baseline facet, and diffs a live facet against the baseline.

Pure functions plus a small CLI used by netcfg-audit.sh (no network, no switch writes):

  netcfg_bridge_qos.py gather-cmd
      print the RouterOS command the orchestrator runs over its one ssh call per switch.
  netcfg_bridge_qos.py attach <flat-json-in> <raw-dir> <switch>...
      read the snapshot JSON (stdin-free: a file), attach each switch's facet parsed from
      <raw-dir>/<switch>.txt, print the result. A reachable switch whose read is incomplete
      fails (exit 1, the switch named on stderr), so --capture never writes a baseline that
      silently lacks the facet.
  netcfg_bridge_qos.py check <baseline.json> <raw-dir> <live-flat-file>
      one finding per line, `<STATUS>\t<1 hard|0 report-only>\t<message>`.

What is baselined (verbatim RouterOS property names):
  bridge_vlan  STATIC entries keyed by `vlan-ids`: sorted `tagged` / `untagged` members +
               `disabled`. The `added by pvid` / `added by mvrp` DYNAMIC entries are runtime
               state (the PVID side is already the bridge port `pvid`).
  bridge_port  per interface: `pvid`, `frame-types`, `disabled` (static ports only).
  qos_profile  per name: `pcp`, `dscp`, `traffic-class`, `automap`, `disabled` (no `hw-id`).
  switch_rule  STATIC rules keyed by their comment (else `#<n>`, n = position among the static
               rules): every property + `disabled`.

DRIFT (hard): a baseline membership missing or changed (tagged <-> untagged), a bridge port
missing or with a changed pvid / frame-types / disabled, a QoS profile missing or changed, a
switch rule missing or changed. Report-only: anything present live but not in the baseline, and
an incomplete read (UNKNOWN) -- a failed read must never page as a removed config.
"""
import json
import os
import re
import shlex
import sys

SECTIONS = ("bridge-vlan", "bridge-port", "qos-profile", "switch-rule")
FACET_KEYS = ("bridge_vlan", "bridge_port", "qos_profile", "switch_rule")
END_MARKER = "=== end"

GATHER_CMD = (
    ':put "=== bridge-vlan"; /interface bridge vlan print detail; '
    ':put "=== bridge-port"; /interface bridge port print detail; '
    ':put "=== qos-profile"; /interface ethernet switch qos profile print detail; '
    ':put "=== switch-rule"; /interface ethernet switch rule print detail; '
    ':put "=== end"'
)

_HEADER = re.compile(r"^\s*(\d+)\s(.*)$")
_KEY = re.compile(r"^[a-z][a-z0-9-]*=")
_FLAG = re.compile(r"^[A-Z*]+$")

PORT_FIELDS = ("pvid", "frame-types")
PROFILE_FIELDS = ("pcp", "dscp", "traffic-class", "automap")


def _split_sections(raw):
    """raw ssh output -> {section: [lines]}, or None when the read did not reach the end marker."""
    lines = raw.replace("\r", "").split("\n")
    if END_MARKER not in (ln.strip() for ln in lines):
        return None
    out, cur = {}, None
    for ln in lines:
        s = ln.strip()
        if s.startswith("=== "):
            name = s[4:].strip()
            if name == "end":
                break
            cur = name
            out[cur] = []
            continue
        if cur is not None:
            out[cur].append(ln)
    if any(sec not in out for sec in SECTIONS):
        return None
    return out


def _entry_blocks(lines):
    """Blank-line separated entry blocks of one `print detail` section (the Flags legend dropped)."""
    blocks, cur = [], []
    for ln in lines:
        if not ln.strip():
            if cur:
                blocks.append(cur)
            cur = []
            continue
        if not cur and ln.startswith("Flags:"):
            continue
        cur.append(ln)
    if cur:
        blocks.append(cur)
    return blocks


def parse_detail(lines):
    """`print detail` lines of one section -> [{"flags": str, "comment": str, "props": dict}].

    RouterOS wraps at 80 columns: an INDENTED line starts a new run of `key=value` tokens, a line
    with NO indent continues the previous one mid-token (a long comment is cut mid-word). The
    comment sits on its own `;;; ` line; values are double-quoted when they need it.
    """
    entries = []
    for block in _entry_blocks(lines):
        m = _HEADER.match(block[0])
        if not m:
            continue
        joined = [m.group(2)]
        for ln in block[1:]:
            if ln[:1].isspace():
                joined.append(ln)
            else:
                joined[-1] += ln
        text = joined[0]
        flags = []
        rest = text.lstrip()
        while rest:
            word = rest.split(None, 1)[0]
            if word.startswith(";;;") or _KEY.match(word) or not _FLAG.match(word):
                break
            flags.append(word)
            rest = rest[len(word):].lstrip()
        comment = ""
        props_text = " ".join(j.strip() for j in joined[1:])
        if rest.startswith(";;;"):
            comment = rest[3:].strip()
        else:
            props_text = (rest + " " + props_text).strip()
        props = {}
        for tok in shlex.split(props_text, posix=True):
            if "=" in tok:
                k, _, v = tok.partition("=")
                props[k] = v
        entries.append({"flags": "".join(flags), "comment": comment, "props": props})
    return entries


def _members(value):
    return sorted(p for p in (value or "").split(",") if p)


def _disabled(entry):
    return "yes" if "X" in entry["flags"] else "no"


def _static(entries):
    return [e for e in entries if "D" not in e["flags"]]


def facet_from_raw(raw):
    """One switch's raw GATHER_CMD output -> its facet dict, or None when the read is incomplete."""
    secs = _split_sections(raw or "")
    if secs is None:
        return None
    vlan = {}
    for e in _static(parse_detail(secs["bridge-vlan"])):
        vid = e["props"].get("vlan-ids", "")
        if not vid:
            continue
        cur = vlan.setdefault(vid, {"tagged": [], "untagged": [], "disabled": _disabled(e)})
        cur["tagged"] = sorted(set(cur["tagged"]) | set(_members(e["props"].get("tagged"))))
        cur["untagged"] = sorted(set(cur["untagged"]) | set(_members(e["props"].get("untagged"))))
    ports = {}
    for e in _static(parse_detail(secs["bridge-port"])):
        iface = e["props"].get("interface", "")
        if iface:
            ports[iface] = {f: e["props"].get(f, "") for f in PORT_FIELDS}
            ports[iface]["disabled"] = _disabled(e)
    profiles = {}
    for e in _static(parse_detail(secs["qos-profile"])):
        name = e["props"].get("name", "")
        if name:
            profiles[name] = {f: e["props"].get(f, "") for f in PROFILE_FIELDS}
            profiles[name]["disabled"] = _disabled(e)
    rules = {}
    for n, e in enumerate(_static(parse_detail(secs["switch-rule"]))):
        key = e["comment"] or "#%d" % n
        rule = dict(e["props"])
        rule["disabled"] = _disabled(e)
        rules[key] = rule
    return {"bridge_vlan": vlan, "bridge_port": ports, "qos_profile": profiles, "switch_rule": rules}


def _membership(vlan_table):
    """{vlan-ids: entry} -> {(vlan-ids, port): "tagged" | "untagged"} (a disabled entry has none)."""
    out = {}
    for vid, e in vlan_table.items():
        if e.get("disabled") == "yes":
            continue
        for p in e.get("untagged", []):
            out[(vid, p)] = "untagged"
        for p in e.get("tagged", []):
            out[(vid, p)] = "tagged"
    return out


def _rule_where(node, rule):
    ports = rule.get("ports", "")
    return "%s %s" % (node, ports) if ports else "%s (all ports)" % node


def _diff_fields(base, live):
    return [k for k in sorted(set(base) | set(live)) if base.get(k, "") != live.get(k, "")]


def check_node(node, base, live):
    """Diff one switch's live facet against its baseline facet.

    Returns [(status, hard, message)]: hard findings carry status DRIFT, report-only ones an
    informational status. `base` None/empty (no facet captured yet) checks nothing; `live` None
    (incomplete read) is one report-only UNKNOWN, never a DRIFT.
    """
    if not base or not any(base.get(k) for k in FACET_KEYS):
        return []
    if live is None:
        return [("UNKNOWN", False,
                 "[report-only UNKNOWN] %s: bridge VLAN / port / QoS read incomplete -- "
                 "not checked this pass" % node)]
    out = []

    def drift(msg):
        out.append(("DRIFT", True, "[DRIFT] " + msg))

    def note(kind, msg):
        out.append(("ADDED", False, "[report-only %s] %s" % (kind, msg)))

    bm = _membership(base.get("bridge_vlan", {}))
    lm = _membership(live.get("bridge_vlan", {}))
    for (vid, port) in sorted(set(bm) | set(lm)):
        b, lv = bm.get((vid, port)), lm.get((vid, port))
        if b == lv:
            continue
        if b is None:
            note("vlan-added", "%s %s: VLAN %s membership %s not in the baseline"
                 % (node, port, vid, lv))
        else:
            drift("%s %s: VLAN %s membership baseline=%s live=%s"
                  % (node, port, vid, b, lv or "none"))

    bp, lp = base.get("bridge_port", {}), live.get("bridge_port", {})
    for iface in sorted(set(bp) | set(lp)):
        if iface not in lp:
            b = bp[iface]
            drift("%s %s: no longer a bridge port (baseline pvid=%s frame-types=%s)"
                  % (node, iface, b.get("pvid", ""), b.get("frame-types", "")))
        elif iface not in bp:
            note("bridge-port-added", "%s %s: bridge port not in the baseline (pvid=%s frame-types=%s)"
                 % (node, iface, lp[iface].get("pvid", ""), lp[iface].get("frame-types", "")))
        else:
            for f in _diff_fields(bp[iface], lp[iface]):
                drift("%s %s: bridge port %s baseline='%s' live='%s'"
                      % (node, iface, f, bp[iface].get(f, ""), lp[iface].get(f, "")))

    bq, lq = base.get("qos_profile", {}), live.get("qos_profile", {})
    for name in sorted(set(bq) | set(lq)):
        if name not in lq:
            drift("%s: QoS profile '%s' missing (baseline dscp=%s traffic-class=%s)"
                  % (node, name, bq[name].get("dscp", ""), bq[name].get("traffic-class", "")))
        elif name not in bq:
            note("qos-profile-added", "%s: QoS profile '%s' not in the baseline" % (node, name))
        else:
            for f in _diff_fields(bq[name], lq[name]):
                drift("%s: QoS profile '%s' %s baseline='%s' live='%s'"
                      % (node, name, f, bq[name].get(f, ""), lq[name].get(f, "")))

    br, lr = base.get("switch_rule", {}), live.get("switch_rule", {})
    for key in sorted(set(br) | set(lr)):
        if key not in lr:
            drift("%s: switch rule '%s' missing (baseline %s)"
                  % (_rule_where(node, br[key]), key,
                     " ".join("%s=%s" % kv for kv in sorted(br[key].items()))))
        elif key not in br:
            note("switch-rule-added", "%s: switch rule '%s' not in the baseline"
                 % (_rule_where(node, lr[key]), key))
        else:
            for f in _diff_fields(br[key], lr[key]):
                drift("%s: switch rule '%s' %s baseline='%s' live='%s'"
                      % (_rule_where(node, br[key]), key, f, br[key].get(f, ""), lr[key].get(f, "")))
    return out


def _read_raw(raw_dir, node):
    path = os.path.join(raw_dir, node + ".txt")
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return ""


def _live_flat(path):
    out = {}
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            k, sep, v = line.rstrip("\n").partition("=")
            if sep:
                out.setdefault(k, v)
    return out


def cmd_attach(json_in, raw_dir, switches):
    with open(json_in, encoding="utf-8") as fh:
        snap = json.load(fh)
    nodes = snap.setdefault("nodes", {})
    failed = []
    for node in switches:
        if nodes.get(node, {}).get("reachable") != "true":
            continue
        facet = facet_from_raw(_read_raw(raw_dir, node))
        if facet is None:
            failed.append(node)
            continue
        nodes[node].update(facet)
    if failed:
        sys.stderr.write("netcfg-audit: bridge VLAN / port / QoS read incomplete on: %s\n"
                         % " ".join(failed))
        return 1
    print(json.dumps(snap, indent=2, sort_keys=True))
    return 0


def cmd_check(baseline_path, raw_dir, live_flat_path):
    with open(baseline_path, encoding="utf-8") as fh:
        nodes = json.load(fh).get("nodes", {})
    live = _live_flat(live_flat_path)
    for node in sorted(nodes):
        base = nodes[node]
        if not any(base.get(k) for k in FACET_KEYS):
            continue
        if live.get(node + ".reachable") != "true":
            continue  # an unreachable switch is reported by the per-port ABSENT lines
        for status, hard, msg in check_node(node, base, facet_from_raw(_read_raw(raw_dir, node))):
            print("%s\t%d\t%s" % (status, 1 if hard else 0, msg))
    return 0


def main(argv):
    if len(argv) >= 2 and argv[1] == "gather-cmd":
        print(GATHER_CMD)
        return 0
    if len(argv) >= 4 and argv[1] == "attach":
        return cmd_attach(argv[2], argv[3], argv[4:])
    if len(argv) == 5 and argv[1] == "check":
        return cmd_check(argv[2], argv[3], argv[4])
    sys.stderr.write("usage: netcfg_bridge_qos.py gather-cmd | attach <json> <raw-dir> <switch>... "
                     "| check <baseline.json> <raw-dir> <live-flat>\n")
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
