#!/usr/bin/env python3
"""issue 1372 -- name every dantesync node that is NOT in the fleet table.

Owner 13.10.2026: "ako to ze na iem pc nei je upgradovany dantesync?!!! udriziavas flotilu pod jednotnou
verziou". The SNV IEM PC and the SNV Companion box ran dantesync outside scripts/lib/dantesync-fleet.sh,
so no roll, version gate or clock watchdog ever reached them; nothing looked for a node outside the table.

This scan probes :8898/status on every host of the given /24 nets (the declared
DANTESYNC_FLEET_SCAN_NETS, dev1's view: the PP LAN is 10.76.x through companion-pp) and prints one line
`UNKNOWN <ip> <name>` per answering node whose address is not in --known. Nothing printed = every
dantesync node is a fleet node. `dantesync-version-gate.sh --fleet` calls it and reports the lines
(report-only). Read-only: one TCP connect + one HTTP GET per host, 64 in parallel.

  python3 scripts/dantesync_fleet_scan.py --nets "10.77.9 10.77.8" --known 10.77.9.62 10.77.9.202 ...
"""
import argparse
import concurrent.futures
import json
import socket
import sys
import urllib.request

PORT = 8898


def hosts(nets):
    """Every host address .1-.254 of each `A.B.C` /24 net, in order."""
    return ["%s.%d" % (n, i) for n in nets for i in range(1, 255)]


def probe(ip, connect_s=0.6, read_s=3.0):
    """The node's name (its PTR, else `?`) when :8898/status answers with JSON, else None."""
    try:
        with socket.create_connection((ip, PORT), timeout=connect_s):
            pass
        with urllib.request.urlopen("http://%s:%d/status" % (ip, PORT), timeout=read_s) as r:
            json.load(r)
    except (OSError, ValueError):
        return None
    try:
        return socket.gethostbyaddr(ip)[0]
    except OSError:
        return "?"


def unknown_nodes(candidates, known, probe=probe, workers=64):
    """[(ip, name)] of every candidate outside `known` whose probe answers, sorted by address."""
    todo = [ip for ip in candidates if ip not in known]
    with concurrent.futures.ThreadPoolExecutor(max(1, workers)) as ex:
        names = list(ex.map(probe, todo))
    rows = [(ip, n) for ip, n in zip(todo, names) if n]
    return sorted(rows, key=lambda r: tuple(int(x) for x in r[0].split(".")))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--nets", required=True, help="space-separated A.B.C /24 nets")
    ap.add_argument("--known", nargs="*", default=[], help="addresses of the fleet nodes")
    a = ap.parse_args(argv)
    for ip, name in unknown_nodes(hosts(a.nets.split()), set(a.known)):
        print("UNKNOWN %s %s" % (ip, name))
    return 0


if __name__ == "__main__":
    sys.exit(main())
