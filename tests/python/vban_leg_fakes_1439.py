"""Fakes for tests/python/test_vban_leg_1439.py (not collected: no `test_` prefix).

One JSON state file (env FAKE_STATE) is shared by fake `nmcli`, `ip`, `nft`, `systemctl` and `sysctl`
on PATH, so what the lib does through one tool is what it reads back through another:
  * nmcli keeps the NetworkManager profiles and the profile APPLIED to each device (a `modify` changes
    only the saved profile; `device reapply` / `connection up` copy it onto the device, like NM);
  * ip derives the device's addresses, routes and rules from the APPLIED profile only;
  * nft keeps the loaded table: `-f FILE` loads it, `list table` prints the real nft 1.0.9 render
    (tests/fixtures/vban_leg_1439/nft-list-table-netns.txt) with the counters the test scripted;
  * sysctl `-p FILE` writes the keys into the fake /proc/sys tree (env FAKE_PROC_SYS).
Every call is logged as one argv line into state["log"].
"""
import json
import os
import sys
from pathlib import Path

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures" / "vban_leg_1439"
NETNS_RENDER = FIXTURES / "nft-list-table-netns.txt"

NMCLI = r'''
import json, os, sys, uuid as _uuid
p = os.environ["FAKE_STATE"]; s = json.load(open(p)); a = sys.argv[1:]
s["log"].append(["nmcli"] + a)
def save(): json.dump(s, open(p, "w"))
def prof(ref):
    for x in s["profiles"]:
        if x["uuid"] == ref or x["props"].get("connection.id") == ref:
            return x
    return None
def out(t):
    sys.stdout.write(t)
if a[:1] == ["--wait"]:
    a = a[2:]
if a[:1] == ["-g"]:
    fields = a[1]; rest = a[2:]
    if rest[:2] == ["connection", "show"] and len(rest) == 2:
        for x in s["profiles"]:
            dev = next((d for d, u in s["active"].items() if u == x["uuid"]), "")
            vals = {"UUID": x["uuid"], "TYPE": x["type"], "DEVICE": dev, "NAME": x["props"].get("connection.id", "")}
            out(":".join(vals[f] for f in fields.split(",")) + "\n")
    elif rest == ["connection", "show", "--active"]:
        for d, u in s["active"].items():
            out(":".join({"UUID": u, "DEVICE": d}[f] for f in fields.split(",")) + "\n")
    elif rest[:2] == ["connection", "show"]:
        x = prof(rest[2])
        if x is None:
            sys.stderr.write("Error: no such connection profile\n"); save(); sys.exit(10)
        v = x["props"].get(fields, "")
        if fields == "802-3-ethernet.mac-address":
            v = v.upper().replace(":", "\\:")
        out(v + "\n")
    else:
        save(); sys.exit(2)
    save(); sys.exit(0)
cmd = a[:2]
if cmd == ["connection", "modify"]:
    x = prof(a[2]); kv = a[3:]
    for i in range(0, len(kv), 2):
        x["props"][kv[i]] = kv[i + 1]
elif cmd == ["connection", "add"]:
    kv = a[2:]; new = {"uuid": str(_uuid.uuid4()), "type": "802-3-ethernet", "props": {
        "ipv4.method": "auto", "connection.autoconnect": "yes", "ipv6.method": "auto"}}
    i = 0
    while i < len(kv):
        k, v = kv[i], kv[i + 1]
        if k == "type": new["type"] = "vlan" if v == "vlan" else "802-3-ethernet"
        elif k == "con-name": new["props"]["connection.id"] = v
        elif k == "ifname": new["props"]["connection.interface-name"] = v
        else: new["props"][k] = v
        i += 2
    s["profiles"].append(new)
elif cmd == ["connection", "delete"]:
    x = prof(a[2]); s["profiles"].remove(x)
    for d in [d for d, u in s["active"].items() if u == x["uuid"]]:
        del s["active"][d]; s["applied"].pop(d, None)
elif cmd == ["device", "reapply"]:
    d = a[2]
    if d not in s["active"] or s.get("reapply_fails"):
        sys.stderr.write("Error: Reapplying connection failed: Can't reapply changes\n"); save(); sys.exit(10)
    s["applied"][d] = dict(prof(s["active"][d])["props"])
elif cmd == ["connection", "up"]:
    x = prof(a[2]); d = a[a.index("ifname") + 1] if "ifname" in a else x["props"]["connection.interface-name"]
    if not s["carrier"].get(d, True):
        sys.stderr.write("Error: Connection activation failed: No suitable device found (no carrier)\n")
        save(); sys.exit(4)
    for dd in [dd for dd, u in s["active"].items() if u == x["uuid"]]:
        del s["active"][dd]
    s["active"][d] = x["uuid"]; s["applied"][d] = dict(x["props"])
elif cmd == ["connection", "down"]:
    x = prof(a[2])
    for d in [d for d, u in s["active"].items() if u == x["uuid"]]:
        del s["active"][d]; s["applied"].pop(d, None)
else:
    save(); sys.exit(2)
save()
'''

IP = r'''
import json, os, sys
p = os.environ["FAKE_STATE"]; s = json.load(open(p)); a = sys.argv[1:]
s["log"].append(["ip"] + a); json.dump(s, open(p, "w"))
def addrs(d):
    pr = s["applied"].get(d)
    if not pr: return []
    if pr.get("ipv4.method") == "auto": return [s["dhcp"][d]]
    if pr.get("ipv4.method") == "manual":
        return [x.strip() for x in pr.get("ipv4.addresses", "").split(",") if x.strip()]
    return []
out = sys.stdout.write
if a == ["-o", "-4", "addr", "show"]:
    for d in s["applied"]:
        for x in addrs(d):
            out("5: %s    inet %s scope global %s\\       valid_lft forever preferred_lft forever\n" % (d, x, d))
    sys.exit(0)
if a[:4] == ["-4", "-o", "addr", "show"]:
    d = a[a.index("dev") + 1]
    for x in addrs(d):
        out("5: %s    inet %s scope global noprefixroute %s\\       valid_lft forever preferred_lft forever\n" % (d, x, d))
elif a == ["-4", "route", "show", "default"]:
    out("default via 10.77.8.1 dev enp56s0 proto static metric 100 \n")
    for d, pr in s["applied"].items():
        if pr.get("ipv4.method") == "auto" and pr.get("ipv4.never-default") != "yes":
            out("default via 10.77.7.1 dev %s proto dhcp src %s metric 101 \n" % (d, s["dhcp"][d].split("/")[0]))
    for d in s.get("extra_default", []):
        out("default via 10.77.7.1 dev %s proto static metric 50 \n" % d)
elif a[:4] == ["-4", "route", "show", "dev"]:
    d = a[4]
    for x in addrs(d):
        ip, plen = x.split("/")
        if plen != "32" and s["applied"][d].get("ipv4.method") == "auto":
            out("10.77.7.0/%s proto kernel scope link src %s metric 101 \n" % (plen, ip))
elif a == ["-4", "rule", "show"]:
    out("0:\tfrom all lookup local\n")
    for d, pr in s["applied"].items():
        for r in [r.strip() for r in pr.get("ipv4.routing-rules", "").split(",") if r.strip()]:
            t = r.split()
            out("%s:\tfrom %s lookup %s proto static\n" % (t[1], t[3], t[5]))
    out("32766:\tfrom all lookup main\n32767:\tfrom all lookup default\n")
elif a[:5] == ["-4", "route", "show", "table", "70"]:
    for d, pr in s["applied"].items():
        for r in [r.strip() for r in pr.get("ipv4.routes", "").split(",") if r.strip()]:
            net, tbl = r.split()
            if tbl == "table=70":
                out("%s dev %s proto static scope link metric 101 \n" % (net, d))
else:
    sys.exit(2)
'''

NFT = r'''
import json, os, re, sys
p = os.environ["FAKE_STATE"]; s = json.load(open(p)); a = sys.argv[1:]
s["log"].append(["nft"] + a)
def save(): json.dump(s, open(p, "w"))
if a[:1] == ["-f"]:
    s["nft_loaded_file"] = open(a[1]).read(); s["nft_render"] = s["netns_render"]
    save(); sys.exit(0)
if a == ["list", "table", "inet", "vban_dante70"]:
    if s.get("nft_unreadable"):
        sys.stderr.write("Error: Operation not permitted\n"); save(); sys.exit(1)
    r = s.get("nft_render")
    if r is None:
        sys.stderr.write("Error: No such file or directory\n"); save(); sys.exit(1)
    i = s["nft_calls"]; s["nft_calls"] = i + 1
    seq = s["nft_counters"]; acc, off = seq[min(i, len(seq) - 1)]
    lines = r.split("\n")
    lines = [re.sub(r"counter packets \d+ bytes \d+ accept", "counter packets %d bytes %d accept" % (acc, acc * 1490), l)
             if "udp dport" in l and "accept" in l and "saddr" in l else l for l in lines]
    lines = [re.sub(r"counter packets \d+ bytes \d+$", "counter packets %d bytes %d" % (off, off * 1490), l)
             if "!=" in l else l for l in lines]
    sys.stdout.write("\n".join(lines))
    save(); sys.exit(0)
save(); sys.exit(2)
'''

SYSTEMCTL = r'''
import json, os, sys
p = os.environ["FAKE_STATE"]; s = json.load(open(p)); a = sys.argv[1:]
s["log"].append(["systemctl"] + a)
def save(): json.dump(s, open(p, "w"))
args = [x for x in a if not x.startswith("-")]
verb = args[0] if args else ""
u = s["units"].setdefault(args[1] if len(args) > 1 else "", {"enabled": False, "active": False})
if verb == "is-enabled":
    if "-q" not in a: print("enabled" if u["enabled"] else "disabled")
    save(); sys.exit(0 if u["enabled"] else 1)
if verb == "is-active":
    if "-q" not in a: print("active" if u["active"] else "inactive")
    save(); sys.exit(0 if u["active"] else 3)
if verb == "enable": u["enabled"] = True
elif verb in ("restart", "start"):
    u["active"] = True
    if s.get("unit_runs_nft"):
        f = s["unit_runs_nft"]
        s["nft_loaded_file"] = open(f).read(); s["nft_render"] = s["netns_render"]
elif verb == "daemon-reload": pass
save()
'''

SYSCTL = r'''
import json, os, sys
p = os.environ["FAKE_STATE"]; s = json.load(open(p)); a = sys.argv[1:]
s["log"].append(["sysctl"] + a); json.dump(s, open(p, "w"))
if "-p" in a:
    root = os.environ["FAKE_PROC_SYS"]
    for line in open(a[a.index("-p") + 1]):
        line = line.strip()
        if not line or line.startswith("#"): continue
        k, v = [x.strip() for x in line.split("=", 1)]
        f = os.path.join(root, k.replace(".", "/") if "/" not in k else k)
        os.makedirs(os.path.dirname(f), exist_ok=True)
        open(f, "w").write(v + "\n")
    sys.exit(0)
sys.exit(2)
'''


def install(bin_dir, state_path, proc_sys):
    """Write the fakes into BIN_DIR; return the env additions."""
    bin_dir = Path(bin_dir)
    bin_dir.mkdir(parents=True, exist_ok=True)
    for name, body in (("nmcli", NMCLI), ("ip", IP), ("nft", NFT), ("systemctl", SYSTEMCTL), ("sysctl", SYSCTL)):
        f = bin_dir / name
        f.write_text("#!%s\n%s" % (sys.executable, body))
        f.chmod(0o755)
    return {"FAKE_STATE": str(state_path), "FAKE_PROC_SYS": str(proc_sys)}


def new_state(path, **over):
    s = {"profiles": [], "active": {}, "applied": {}, "carrier": {}, "dhcp": {},
         "nft_render": None, "nft_counters": [[0, 0]], "nft_calls": 0, "units": {},
         "netns_render": NETNS_RENDER.read_text(), "log": []}
    s.update(over)
    Path(path).write_text(json.dumps(s))
    return s


def read_state(path):
    return json.loads(Path(path).read_text())


def calls(state, tool, *prefix):
    """Every logged argv of TOOL whose argv (after any `--wait N`) starts with PREFIX."""
    out = []
    for c in state["log"]:
        if c[0] != tool:
            continue
        rest = c[1:]
        if rest[:1] == ["--wait"]:
            rest = rest[2:]
        if list(rest[:len(prefix)]) == list(prefix):
            out.append(rest)
    return out


def write_proc(proc_sys, iface, ignore, announce):
    d = Path(proc_sys) / "net" / "ipv4" / "conf" / iface
    d.mkdir(parents=True, exist_ok=True)
    (d / "arp_ignore").write_text("%s\n" % ignore)
    (d / "arp_announce").write_text("%s\n" % announce)


def os_env_without(keys):
    return {k: v for k, v in os.environ.items() if k not in keys}
