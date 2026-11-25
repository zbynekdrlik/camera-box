#!/usr/bin/env python3
"""issue 1554: the venue network RULE checks of the rig test (SNV + PP), beside the netcfg facet.

Owner 25.11.2026 (network#18): "take overovanie v snv/pp v camerabox rig ci testu chcem mat aby sa
zachytili problemy v sieti, pripadne aj zmeny v konfiguracii a vyvolali fail". So every rule below
that is `hard` FAILS the rig test (scripts/netcfg-rules.sh exits 3); WARN and UNKNOWN only report.

One read per device (scripts/netcfg-rules.sh, one ssh call, never a loop): the verified read
commands of READS between `=== <section>` markers and a closing `=== end` (the issue-1439
GATHER_CMD shape; the section splitter and the `print detail` parser are netcfg_bridge_qos.py's).
Every command is a plain `print` / `monitor ... once` / `export`: PP is administered by a third
party who reads the device logs, so nothing here may ever write or guess.

The venue facts (devices, Dante subnet, rules) are scripts/netcfg-venues.json. The repo is
PUBLIC and the PP config is not ours: a finding names the RouterOS path + the entry's key, never a
configuration value, and the drift baseline commits only a hash per device
(scripts/netcfg-export-baseline.json); the export text stays on dev1.

Rules (the ticket's numbering):
  1  export drift: the normalised `/export terse` hash per device vs the committed hash  FAIL
  2  IGMP: policy "off" = no bridge with igmp-snooping=yes; policy "querier" = snooping only
     with an active querier (bridge monitor igmp-querier not `none`)                     FAIL
  3  RSTP: an in-bridge host port with edge-port=no; a bridge link (admit-only-vlan-tagged,
     or a port that receives BPDUs) whose topology-changes grew since the previous run (a
     dev1 state file per venue); a counter that went back = a reboot, rebased             FAIL
  4  every static bridge port HW-offloaded (H flag); WARN on a device marked cpu_bridge   FAIL
  5  Dante QoS on a qos device: trust-l3=keep on every bridge port, a profile per expected
     DSCP with the venue's traffic class                                                FAIL
  7  Dante DHCP: the pool inside the venue's dhcp_zone (the fixed Dante addresses are set on
     the hosts below / above it), no gateway / DNS on the Dante network where the venue
     says so. A DHCP reservation (static lease) inside the pool is the server's own and fine FAIL
  8  PTP mirror (a device with ptp_mirror facts)                                        FAIL
  10 routerboard firmware upgraded, reboot pending                                       WARN
An unreachable device or an incomplete read FAILS (the gate is fail-closed); a section the parser
does not know is UNKNOWN (report-only), never a pass of its rule.
"""
import hashlib
import ipaddress
import json
import os
import re
import shlex
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)

import netcfg_bridge_qos as nbq  # noqa: E402

COMMANDS = {
    "identity": "/system identity print",
    "routerboard": "/system routerboard print",
    "bridge-monitor": "/interface bridge monitor [find] once",
    "bridge-port": "/interface bridge port print detail",
    "port-monitor": "/interface bridge port monitor [find] once",
    "export": "/export terse",
}
READS = {
    "full": ("identity", "routerboard", "bridge-monitor", "bridge-port", "port-monitor", "export"),
    "router": ("identity", "routerboard", "export"),
}
VERBS = ("add", "set")
TRUNK_FRAME_TYPES = "admit-only-vlan-tagged"
# the entry key a drift line names (never a value of any other property)
ENTRY_KEYS = ("name", "interface", "vlan-ids", "address", "dst-address", "comment")


class FactsError(ValueError):
    """scripts/netcfg-venues.json is not in the shape this module needs."""


# ---------------------------------------------------------------- facts
def load_venue(path, venue):
    with open(path, encoding="utf-8") as fh:
        data = json.load(fh)
    v = data.get("venues", {}).get(venue)
    if not isinstance(v, dict):
        raise FactsError("no venue %r in %s" % (venue, path))
    devices = v.get("devices")
    if not isinstance(devices, list) or not devices:
        raise FactsError("venue %r has no devices" % venue)
    names = set()
    for d in devices:
        if not re.match(r"^[a-z][a-z0-9_]*$", str(d.get("name", ""))):
            raise FactsError("bad device name %r" % d.get("name"))
        if d["name"] in names:
            raise FactsError("device %r listed twice" % d["name"])
        names.add(d["name"])
        if d.get("reads") not in READS:
            raise FactsError("device %r: unknown reads %r" % (d["name"], d.get("reads")))
        if not re.match(r"^[0-9.]+$", str(d.get("host", ""))):
            raise FactsError("device %r: bad host %r" % (d["name"], d.get("host")))
    if v.get("igmp") not in ("off", "querier"):
        raise FactsError("venue %r: igmp must be off or querier" % venue)
    return v


def gather_cmd(reads):
    parts = []
    for sec in READS[reads]:
        parts.append(':put "=== %s"; %s' % (sec, COMMANDS[sec]))
    parts.append(':put "=== end"')
    return "; ".join(parts)


# ---------------------------------------------------------------- parsers
def parse_export(lines):
    """`/export` or `/export terse` lines -> [{"path", "verb", "target", "props"}].

    Handles both forms: a `/path` header line followed by `add ...` / `set ...` lines, and the
    terse one-line `/path verb ...`. A trailing `\\` continues the line. `[ find ... ]` is kept as
    the target text; `set <name>` keeps the bare name as the target. Comment lines are skipped.
    """
    joined, cur = [], ""
    for ln in lines:
        ln = ln.rstrip("\r\n")
        if cur:
            ln = cur + ln.lstrip()
            cur = ""
        if ln.endswith("\\"):
            cur = ln[:-1]
            continue
        joined.append(ln)
    entries, path = [], ""
    for ln in joined:
        s = ln.strip()
        if not s or s.startswith("#"):
            continue
        try:
            words = shlex.split(s, posix=True)
        except ValueError as exc:
            raise nbq.ParseError("unbalanced quote in export: %s" % exc) from exc
        vi = next((i for i, w in enumerate(words) if w in VERBS), None)
        if vi is None:
            if s.startswith("/"):
                path = s
            continue
        if s.startswith("/"):
            path = " ".join(words[:vi])
        rest = words[vi + 1:]
        target, props, i = "", {}, 0
        if rest and rest[0] == "[":
            j = rest.index("]") if "]" in rest else len(rest)
            target = "[ " + " ".join(rest[1:j]) + " ]"
            i = j + 1
        elif rest and "=" not in rest[0]:
            target = rest[0]
            i = 1
        for tok in rest[i:]:
            k, sep, val = tok.partition("=")
            if sep:
                props[k] = val
        entries.append({"path": path, "verb": words[vi], "target": target, "props": props})
    return entries


def parse_kv(lines):
    """`key: value` monitor/print output -> {key: value} (the first value of a wrapped list)."""
    out = {}
    for ln in lines:
        m = re.match(r"^\s*([a-z][a-z0-9-]*):\s*(.*?)\s*$", ln.rstrip("\r\n"))
        if m and m.group(1) not in out:
            out[m.group(1)] = m.group(2)
    return out


def parse_columnar(lines):
    """`/interface bridge port monitor [find] once` -> {port: {key: value}}.

    The columns start where each port name starts on the `interface:` line; every other line's
    value for a port is the text under that column.
    """
    rows = [ln.rstrip("\r\n") for ln in lines]
    head = next((ln for ln in rows if re.match(r"^\s*interface:\s", ln)), None)
    if head is None:
        return {}
    vstart = head.index("interface:") + len("interface:")
    starts = [m.start() for m in re.finditer(r"\S+", head[vstart:])]
    starts = [vstart + s for s in starts]
    ports = head[vstart:].split()
    out = {p: {} for p in ports}
    for ln in rows:
        m = re.match(r"^\s*([a-z][a-z0-9-]*):", ln)
        if not m or m.group(1) == "interface":
            continue
        key = m.group(1)
        for n, p in enumerate(ports):
            end = starts[n + 1] if n + 1 < len(starts) else len(ln)
            val = ln[starts[n]:end].strip() if len(ln) > starts[n] else ""
            out[p].setdefault(key, val)
    return out


def parse_device(raw, reads):
    """One device's raw read -> {"sections": {...}} or None when the read is incomplete."""
    secs = nbq._split_sections(raw or "", required=READS[reads])
    if secs is None:
        return None
    dev = {"sections": secs}
    try:
        dev["export"] = parse_export(secs["export"])
    except nbq.ParseError:
        dev["export"] = None
    dev["routerboard"] = parse_kv(secs["routerboard"])
    if "bridge-monitor" in secs:
        dev["bridge_monitor"] = parse_kv(secs["bridge-monitor"])
    if "bridge-port" in secs:
        try:
            dev["bridge_ports"] = nbq.parse_detail(secs["bridge-port"])
        except nbq.ParseError:
            dev["bridge_ports"] = None
    if "port-monitor" in secs:
        dev["port_monitor"] = parse_columnar(secs["port-monitor"])
    return dev


# ---------------------------------------------------------------- export drift (rule 1)
def export_lines(entries):
    """Normalised export: one canonical line per entry, sorted (order-independent)."""
    out = []
    for e in entries:
        props = " ".join("%s=%s" % (k, e["props"][k]) for k in sorted(e["props"]))
        out.append(" ".join(x for x in (e["path"], e["verb"], e["target"], props) if x))
    return sorted(out)


def export_hash(entries):
    return hashlib.sha256("\n".join(export_lines(entries)).encode()).hexdigest()


def _entry_label(e):
    for k in ENTRY_KEYS:
        if e["props"].get(k):
            return "%s %s %s=%s" % (e["path"], e["verb"], k, e["props"][k])
    if e["target"]:
        return "%s %s %s" % (e["path"], e["verb"], e["target"])
    return "%s %s" % (e["path"], e["verb"])


def export_diff_labels(old_entries, new_entries, limit=12):
    """Path + entry key of every entry that changed, was added or removed -- never a value."""
    old = {}
    for e in old_entries:
        old.setdefault(_entry_label(e), []).append(export_lines([e])[0])
    new = {}
    for e in new_entries:
        new.setdefault(_entry_label(e), []).append(export_lines([e])[0])
    out = []
    for lbl in sorted(set(old) | set(new)):
        a, b = sorted(old.get(lbl, [])), sorted(new.get(lbl, []))
        if a == b:
            continue
        out.append("%s: %s" % (lbl, "added" if not a else "removed" if not b else "changed"))
    if len(out) > limit:
        out = out[:limit] + ["... %d more" % (len(out) - limit)]
    return out


def rule_drift(name, dev, baseline_hash, local_text):
    if dev.get("export") is None:
        return [("UNKNOWN", False, "%s: export unreadable, drift not checked" % name)]
    live = export_hash(dev["export"])
    if not baseline_hash:
        return [("UNKNOWN", False, "%s: no committed export baseline (run --capture, PR the hash)" % name)]
    if live == baseline_hash:
        return []
    detail = ""
    if local_text is not None:
        old = parse_export(local_text.splitlines())
        if export_hash(old) == baseline_hash:
            labels = export_diff_labels(old, dev["export"])
            detail = " -- " + "; ".join(labels) if labels else ""
        else:
            detail = " (the dev1 export copy is not the baselined one, no detail)"
    return [("DRIFT", True, "%s: configuration changed since the baseline%s" % (name, detail))]


# ---------------------------------------------------------------- rules 2-10
def _bridges(dev):
    return [e for e in dev["export"] if e["path"] == "/interface bridge" and e["verb"] == "add"]


def rule_igmp(name, dev, venue):
    if dev.get("export") is None:
        return [("UNKNOWN", False, "%s: export unreadable, IGMP not checked" % name)]
    snooping = [e["props"].get("name", "?") for e in _bridges(dev)
                if e["props"].get("igmp-snooping") == "yes"]
    if not snooping:
        return []
    if venue["igmp"] == "off":
        return [("FAIL", True, "%s: bridge %s has igmp-snooping=yes (venue rule: IGMP snooping off "
                 "on every bridge)" % (name, b)) for b in snooping]
    mon = dev.get("bridge_monitor")
    if mon is None:
        return [("UNKNOWN", False, "%s: igmp-snooping=yes but no bridge monitor read, the querier "
                 "is not checked here" % name)]
    querier = mon.get("igmp-querier", "")
    if not querier or querier == "none":
        return [("FAIL", True, "%s: igmp-snooping=yes with no active IGMP querier (igmp-querier: "
                 "%s) -- multicast (Dante) can stop after the membership timeout"
                 % (name, querier or "absent"))]
    return []


def _static_ports(dev):
    return [e for e in (dev.get("bridge_ports") or []) if "D" not in e["flags"]]


def _trunks(dev):
    return {e["props"].get("interface") for e in _static_ports(dev)
            if e["props"].get("frame-types") == TRUNK_FRAME_TYPES}


def row_restricted(dev, port):
    return any(e["props"].get("interface") == port and e["props"].get("restricted-tcn") == "yes"
               for e in _static_ports(dev))


def _rx_bpdu(row):
    _, sep, rx = row.get("tx-rx-bpdu", "").partition("/")
    return int(rx) if sep and rx.isdigit() else 0


def rule_rstp(name, dev, prev):
    """-> (findings, new state {port: topology-changes})."""
    mon = dev.get("port_monitor")
    if mon is None or dev.get("bridge_ports") is None:
        return [], prev or {}
    if not mon:
        return [("UNKNOWN", False, "%s: bridge port monitor not in a known shape" % name)], prev or {}
    trunks = _trunks(dev)
    out, state = [], {}
    for port, row in sorted(mon.items()):
        if row.get("status") != "in-bridge":
            continue
        if _rx_bpdu(row) > 0:
            trunks.add(port)  # a bridge neighbour answers RSTP here: a link, not a host port
        if port not in trunks and row.get("edge-port") == "no":
            out.append(("FAIL", True, "%s: host port %s is not an RSTP edge port (edge-port: no) -- "
                        "a topology change there flushes the MAC tables" % (name, port)))
        tc = row.get("topology-changes", "")
        if port in trunks and tc.isdigit():
            state[port] = int(tc)
            old = (prev or {}).get(port)
            if old is None:
                continue
            if int(tc) > old and row_restricted(dev, port):
                out.append(("WARN", False, "%s: bridge link %s saw %d new RSTP topology change(s) since "
                            "the previous run; restricted-tcn=yes keeps them out of the MAC tables"
                            % (name, port, int(tc) - old)))
            elif int(tc) > old:
                out.append(("FAIL", True, "%s: bridge link %s saw %d new RSTP topology change(s) since the "
                            "previous run (last %s ago) -- the network#5 Dante dropout cause"
                            % (name, port, int(tc) - old, row.get("last-topology-change") or "?")))
            elif int(tc) < old:
                out.append(("REBASE", False, "%s: bridge link %s topology-changes went back %d -> %s "
                            "(device restarted), rebased" % (name, port, old, tc)))
    return out, state


def rule_hw_offload(name, dev, facts):
    ports = dev.get("bridge_ports")
    if ports is None:
        return [("UNKNOWN", False, "%s: bridge ports unreadable" % name)] if "bridge-port" in dev["sections"] else []
    out = []
    for e in _static_ports(dev):
        if "X" in e["flags"] or "H" in e["flags"]:
            continue
        iface = e["props"].get("interface", "?")
        if facts.get("cpu_bridge"):
            out.append(("WARN", False, "%s: bridge port %s is not HW-offloaded (CPU bridging) -- never "
                        "plug Dante here" % (name, iface)))
        else:
            out.append(("FAIL", True, "%s: bridge port %s is not HW-offloaded (no H flag) -- its "
                        "traffic goes through the CPU" % (name, iface)))
    return out


def rule_qos(name, dev, facts, venue):
    if not facts.get("qos"):
        return []
    if dev.get("export") is None or dev.get("bridge_ports") is None:
        return [("UNKNOWN", False, "%s: QoS not checked (export or bridge ports unreadable)" % name)]
    trust = {e["target"]: e["props"].get("trust-l3") for e in dev["export"]
             if e["path"] == "/interface ethernet switch qos port" and e["verb"] == "set"}
    out = []
    for e in _static_ports(dev):
        iface = e["props"].get("interface", "?")
        if trust.get(iface) != "keep":
            out.append(("FAIL", True, "%s: port %s does not keep the DSCP (trust-l3=keep missing)"
                        % (name, iface)))
    profiles = [e["props"] for e in dev["export"]
                if e["path"] == "/interface ethernet switch qos profile" and e["verb"] == "add"]
    for dscp, tc in sorted(venue.get("dscp_traffic_class", {}).items()):
        if not any(p.get("dscp") == dscp and p.get("traffic-class") == tc for p in profiles):
            out.append(("FAIL", True, "%s: no QoS profile maps DSCP %s to traffic class %s"
                        % (name, dscp, tc)))
    return out


def _ranges(text):
    out = []
    for part in (text or "").split(","):
        lo, sep, hi = part.strip().partition("-")
        try:
            a = ipaddress.ip_address(lo)
            b = ipaddress.ip_address(hi) if sep else a
        except ValueError:
            return None
        out.append((a, b))
    return out


def rule_dhcp(name, dev, facts, venue):
    if not facts.get("dante_dhcp"):
        return []
    if dev.get("export") is None:
        return [("UNKNOWN", False, "%s: export unreadable, Dante DHCP not checked" % name)]
    net = ipaddress.ip_network(venue["dante_subnet"])
    ex = dev["export"]
    ifaces = {e["props"].get("interface") for e in ex
              if e["path"] == "/ip address" and e["verb"] == "add"
              and _in_net(e["props"].get("address", "").split("/")[0], net)}
    servers = [e["props"] for e in ex if e["path"] == "/ip dhcp-server" and e["verb"] == "add"
               and e["props"].get("interface") in ifaces]
    if not servers:
        return [("FAIL", True, "%s: no DHCP server on the Dante network %s" % (name, net))]
    out = []
    lo_zone, hi_zone = venue["dhcp_zone"]
    pools = {e["props"].get("name"): e["props"].get("ranges") for e in ex
             if e["path"] == "/ip pool" and e["verb"] == "add"}
    for srv in servers:
        pool = srv.get("address-pool", "")
        ranges = _ranges(pools.get(pool))
        if not ranges:
            out.append(("FAIL", True, "%s: Dante DHCP server %s has no readable pool %s"
                        % (name, srv.get("name", "?"), pool)))
            continue
        for a, b in ranges:
            if a not in net or b not in net or int(a) - int(net.network_address) < lo_zone \
                    or int(b) - int(net.network_address) > hi_zone:
                out.append(("FAIL", True, "%s: Dante DHCP pool %s %s-%s reaches outside .%d-.%d, where "
                            "the fixed Dante addresses live" % (name, pool, a, b, lo_zone, hi_zone)))
    if venue.get("dante_dhcp_no_gateway"):
        for e in ex:
            if e["path"] == "/ip dhcp-server network" and e["verb"] == "add" \
                    and e["props"].get("address") == str(net):
                for k in ("gateway", "dns-server"):
                    if e["props"].get(k):
                        out.append(("FAIL", True, "%s: the Dante DHCP network %s hands out a %s "
                                    "(venue rule: Dante without gateway / DNS)" % (name, net, k)))
    return out


def _in_net(addr, net):
    try:
        return ipaddress.ip_address(addr) in net
    except ValueError:
        return False


def rule_ptp_mirror(name, dev, facts):
    m = facts.get("ptp_mirror")
    if not m:
        return []
    if dev.get("export") is None or dev.get("bridge_ports") is None:
        return [("UNKNOWN", False, "%s: PTP mirror not checked (export or bridge ports unreadable)" % name)]
    ex, out = dev["export"], []
    rules = [e["props"] for e in ex if e["path"] == "/interface ethernet switch rule" and e["verb"] == "add"]
    if not any(r.get("comment") == m["rule_comment"] and r.get("mirror") == "yes"
               and m["source"] in r.get("ports", "").split(",") for r in rules):
        out.append(("FAIL", True, "%s: PTP mirror rule %s (mirror of %s) missing"
                    % (name, m["rule_comment"], m["source"])))
    if not any(e["path"] == "/interface ethernet switch" and e["verb"] == "set"
               and e["props"].get("mirror-target") == m["target"] for e in ex):
        out.append(("FAIL", True, "%s: switch mirror-target is not %s" % (name, m["target"])))
    ports = {e["props"].get("interface"): e for e in _static_ports(dev)}
    if m["target"] in ports:
        out.append(("FAIL", True, "%s: mirror target %s is inside the bridge" % (name, m["target"])))
    ret = ports.get(m["return_port"])
    if ret is None or ret["props"].get("pvid") != m["return_pvid"]:
        out.append(("FAIL", True, "%s: PTP return port %s not in the bridge with pvid %s"
                    % (name, m["return_port"], m["return_pvid"])))
    mon = dev.get("port_monitor") or {}
    if m["return_port"] in mon and mon[m["return_port"]].get("status") != "in-bridge":
        out.append(("FAIL", True, "%s: PTP return port %s is not running (status %s)"
                    % (name, m["return_port"], mon[m["return_port"]].get("status"))))
    return out


def rule_firmware(name, dev):
    rb = dev.get("routerboard") or {}
    cur, up = rb.get("current-firmware"), rb.get("upgrade-firmware")
    if cur and up and cur != up:
        return [("WARN", False, "%s: routerboard firmware %s -> %s upgraded, reboot pending"
                 % (name, cur, up))]
    return []


# ---------------------------------------------------------------- one venue
def check_venue(venue, raws, baseline, local_texts, prev_state):
    """raws {device: raw text or None}; baseline {device: hash}; local_texts {device: text|None};
    prev_state {device: {port: n}} -> (findings, new_state)."""
    findings, state = [], {}
    for facts in venue["devices"]:
        name = facts["name"]
        dev = parse_device(raws.get(name), facts["reads"])
        if dev is None:
            findings.append(("FAIL", True, "%s (%s): unreachable or incomplete read -- the network "
                             "check cannot pass without it" % (name, facts["host"])))
            if name in (prev_state or {}):
                state[name] = prev_state[name]
            continue
        findings += rule_drift(name, dev, (baseline or {}).get(name), (local_texts or {}).get(name))
        findings += rule_igmp(name, dev, venue)
        tc_findings, tc_state = rule_rstp(name, dev, (prev_state or {}).get(name))
        findings += tc_findings
        if tc_state:
            state[name] = tc_state
        findings += rule_hw_offload(name, dev, facts)
        findings += rule_qos(name, dev, facts, venue)
        findings += rule_dhcp(name, dev, facts, venue)
        findings += rule_ptp_mirror(name, dev, facts)
        findings += rule_firmware(name, dev)
    return findings, state


# ---------------------------------------------------------------- CLI
def _read(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return None


def _load_json(path, default):
    text = _read(path)
    if text is None:
        return default
    try:
        return json.loads(text)
    except ValueError:
        return default


def cmd_devices(facts_path, venue_name):
    v = load_venue(facts_path, venue_name)
    a = v.get("access", {})
    def field(p, expand=False):
        # "-" for an empty field: bash `read` with a tab IFS merges empty tab-separated fields
        return (os.path.expanduser(p) if expand else p) if p else "-"
    print("ACCESS\t%s\t%s\t%s\t%s" % (field(a.get("user", "")), field(a.get("key", ""), True),
                                      field(a.get("password_file", ""), True), field(a.get("jump", ""))))
    for d in v["devices"]:
        print("DEVICE\t%s\t%s\t%s" % (d["name"], d["host"], gather_cmd(d["reads"])))
    return 0


def cmd_check(facts_path, venue_name, raw_dir, baseline_path, export_dir, state_path):
    v = load_venue(facts_path, venue_name)
    raws = {d["name"]: _read(os.path.join(raw_dir, d["name"] + ".txt")) for d in v["devices"]}
    baseline = _load_json(baseline_path, {}).get(venue_name, {})
    local = {d["name"]: _read(os.path.join(export_dir, d["name"] + ".rsc")) for d in v["devices"]}
    prev = _load_json(state_path, {})
    findings, state = check_venue(v, raws, baseline, local, prev)
    for status, hard, msg in findings:
        print("%s\t%d\t%s" % (status, 1 if hard else 0, msg))
    if state_path:
        tmp = state_path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(state, fh, indent=2, sort_keys=True)
        os.replace(tmp, state_path)
    return 0


def cmd_capture(facts_path, venue_name, raw_dir, baseline_path, export_dir):
    """Write each device's export text to export_dir and its hash into the baseline JSON.

    An incomplete read writes nothing for any device (exit 1, the device named on stderr), so a
    baseline never silently loses a device.
    """
    v = load_venue(facts_path, venue_name)
    parsed = {}
    for d in v["devices"]:
        raw = _read(os.path.join(raw_dir, d["name"] + ".txt"))
        dev = parse_device(raw, d["reads"])
        if dev is None or dev.get("export") is None:
            sys.stderr.write("netcfg-rules: %s read incomplete, nothing captured\n" % d["name"])
            return 1
        parsed[d["name"]] = (dev, "\n".join(dev["sections"]["export"]) + "\n")
    os.makedirs(export_dir, mode=0o700, exist_ok=True)
    data = _load_json(baseline_path, {})
    data.setdefault("_comment", "issue 1554: sha256 of each device's normalised `/export terse` "
                    "(scripts/netcfg_rules.py export_hash). The text itself stays on dev1 "
                    "(~/.camera-box/netcfg-export/<venue>/), never in this public repo. A legitimate "
                    "change = scripts/netcfg-rules.sh --capture --venue <v> + a PR of this file.")
    hashes = {}
    for name, (dev, text) in parsed.items():
        with open(os.path.join(export_dir, name + ".rsc"), "w", encoding="utf-8") as fh:
            fh.write(text)
        hashes[name] = export_hash(dev["export"])
    data[venue_name] = hashes
    tmp = baseline_path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=2, sort_keys=True)
        fh.write("\n")
    os.replace(tmp, baseline_path)
    for name in sorted(hashes):
        print("captured %s %s" % (name, hashes[name]))
    return 0


def main(argv):
    try:
        if len(argv) == 4 and argv[1] == "devices":
            return cmd_devices(argv[2], argv[3])
        if len(argv) == 8 and argv[1] == "check":
            return cmd_check(*argv[2:8])
        if len(argv) == 7 and argv[1] == "capture":
            return cmd_capture(*argv[2:7])
    except (FactsError, OSError, ValueError) as exc:
        sys.stderr.write("netcfg-rules: %s\n" % exc)
        return 2
    sys.stderr.write("usage: netcfg_rules.py devices <facts> <venue>\n"
                     "       netcfg_rules.py check <facts> <venue> <raw-dir> <baseline> <export-dir> <state>\n"
                     "       netcfg_rules.py capture <facts> <venue> <raw-dir> <baseline> <export-dir>\n")
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
