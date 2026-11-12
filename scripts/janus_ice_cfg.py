#!/usr/bin/env python3
"""Janus's ICE gathering list in /etc/janus/janus.jcfg (camera-box issue 1499).

dstack's two-networks rule (section 4): a box announces a non-Dante service on its LAN or loopback
address only. Janus (the web intercom's media gateway) gathers ICE host candidates on EVERY interface by
default, so on a box with an audio-VLAN leg (strih-snv: enp7s0.70 = 10.77.7.202) a phone receives the
Dante-network address as a candidate. The fix is one setting in the `nat:` section, inside one managed
block:

    nat: {
        # camera-box issue 1499: ICE candidates on the LAN address only (setup-strih step 14) -- managed, do not edit
        ice_enforce_list = "10.77.9.202"
        # end camera-box issue 1499
        ...the package's own lines, untouched (its ice_ignore_list = "vmnet" is ignored once an
           enforce list is set)...
    }

Only the managed block is ever written or removed; every other line stays byte-identical. An
`ice_enforce_list` set by hand outside the block is a CONFLICT (libconfig refuses a duplicate setting,
so Janus would not start): exit 2, the file untouched. The block lives next to the issue-1453 TURN REST
block (scripts/janus_turn_rest_cfg.py), whose libconfig helpers it reuses.

    janus_ice_cfg.py --file F --set ADDR   -> the block present with ADDR   (prints changed|unchanged)
    janus_ice_cfg.py --file F --clear      -> the block absent              (prints changed|unchanged)
    janus_ice_cfg.py --file F --check ADDR -> rc 0 iff the block is present with ADDR (prints ok|absent|differs)

Exit 1 on a read/write error, 2 on a conflict, a malformed nat section or an unusable address.
"""
import argparse
import ipaddress
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from janus_turn_rest_cfg import JcfgError, _nat_span, _write  # noqa: E402

BEGIN = ("# camera-box issue 1499: ICE candidates on the LAN address only (setup-strih step 14)"
         " -- managed, do not edit")
END = "# end camera-box issue 1499"
FOREIGN = re.compile(r"^\s*ice_enforce_list\s*=")


def _strip_block(lines):
    out, inside = [], False
    for ln in lines:
        s = ln.strip()
        if s == BEGIN:
            inside = True
            continue
        if inside and s == END:
            inside = False
            continue
        if not inside:
            out.append(ln)
    if inside:
        raise JcfgError("the managed ICE block has no end marker -- fix the file by hand")
    return out


def _block(addr, indent):
    return [indent + BEGIN + "\n",
            '%sice_enforce_list = "%s"\n' % (indent, addr),
            indent + END + "\n"]


def _usable(addr):
    try:
        ip = ipaddress.IPv4Address(addr)
    except ValueError:
        return False
    return not (ip.is_loopback or ip.is_unspecified or ip.is_multicast)


def apply(text, addr):
    """The new file text: the managed block present with `addr`, or absent when `addr` is None."""
    if addr is not None and not _usable(addr):
        raise JcfgError("not a usable LAN IPv4 address: %r" % addr)
    lines = _strip_block(text.splitlines(keepends=True))
    span = _nat_span(lines)
    if span is not None:
        for ln in lines[span[0] + 1:span[1]]:
            if FOREIGN.match(ln):
                raise JcfgError("nat already sets %s by hand -- remove it (this tool manages it)" % ln.strip())
    if addr is None:
        return "".join(lines)
    if span is None:
        if lines and not lines[-1].endswith("\n"):
            lines[-1] += "\n"
        return "".join(lines + ["\n", "nat: {\n"] + _block(addr, "\t") + ["}\n"])
    first = lines[span[0] + 1] if span[0] + 1 < span[1] else "\t"
    indent = first[:len(first) - len(first.lstrip())] or "\t"
    if indent.strip():
        indent = "\t"
    return "".join(lines[:span[0] + 1] + _block(addr, indent) + lines[span[0] + 1:])


def check(text, addr):
    """ok | absent | differs: whether the managed block is present inside nat with exactly `addr`."""
    lines = text.splitlines()
    starts = [i for i, ln in enumerate(lines) if ln.strip() == BEGIN]
    if not starts:
        return "absent"
    i = starts[0]
    body = [ln.strip() for ln in lines[i + 1:i + 3]]
    want = ['ice_enforce_list = "%s"' % addr, END]
    span = _nat_span(text.splitlines(keepends=True))
    inside = span is not None and span[0] < i < span[1]
    foreign = span is not None and any(FOREIGN.match(ln) for ln in lines[span[0] + 1:span[1]]
                                       if ln.strip() != want[0])
    return "ok" if body == want and inside and len(starts) == 1 and not foreign else "differs"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--file", required=True)
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--set", metavar="ADDR")
    g.add_argument("--clear", action="store_true")
    g.add_argument("--check", metavar="ADDR")
    a = ap.parse_args(argv)
    try:
        with open(a.file) as f:
            text = f.read()
    except OSError as e:
        print("janus_ice_cfg: cannot read %s: %s" % (a.file, e), file=sys.stderr)
        return 1
    try:
        if a.check is not None:
            verdict = check(text, a.check)
            print(verdict)
            return 0 if verdict == "ok" else 1
        new = apply(text, None if a.clear else a.set)
    except JcfgError as e:
        print("janus_ice_cfg: %s: %s" % (a.file, e), file=sys.stderr)
        return 2
    if new == text:
        print("unchanged")
        return 0
    try:
        _write(a.file, new)
    except OSError as e:
        print("janus_ice_cfg: cannot write %s: %s" % (a.file, e), file=sys.stderr)
        return 1
    print("changed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
