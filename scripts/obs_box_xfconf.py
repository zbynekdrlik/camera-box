#!/usr/bin/env python3
"""Merge and grade declared properties in an xfconf channel file (issue 1466, the gradual move to XFCE).

An xfconf channel file (~/.config/xfce4/xfconf/xfce-perchannel-xml/<channel>.xml) is rewritten by the
running XFCE apps themselves: xfwm4 stores every one of its settings after its first start, the panel adds
its plugin defaults, xfsettingsd rewrites the keyboard shortcuts. So setup-strih never writes such a file
whole and verify-strih never compares its bytes. Setup MERGES the box's declared properties into the
file (every other property kept) and verify checks that each declared property is there with its value.

The declaration (the SPEC) is one property per line, tab-separated:

    /path/to/property<TAB>TYPE<TAB>VALUE

TYPE is an xfconf scalar type (string, bool, int, uint, double), `array:<type>` with the VALUE elements
separated by commas, or `replace`: the property's child properties are removed before the rest of the
spec is applied (merge only), so a subtree such as the panel's /plugins holds exactly what the spec
declares. Blank lines and `#` lines are ignored.

Usage:
    obs_box_xfconf.py merge CHANNEL FILE SPEC   -> the merged XML on stdout (FILE may be missing)
    obs_box_xfconf.py check FILE SPEC           -> rc 0 + `ok`, rc 1 + the first difference

    obs_box_xfconf.py apply CHANNEL SPEC        -> set SPEC through a RUNNING xfconfd with xfconf-query (run
                                                it as the desktop user on its session bus), then read every
                                                property back: rc 0 + `ok`, rc 1 + the first failure
    obs_box_xfconf.py verify-live CHANNEL SPEC  -> the read-back alone (e.g. after an xfce4-panel restart)

SPEC is a file, or `-` for stdin. A running xfconfd owns its channels and writes them from memory, so a
live box is changed through it (apply), never by rewriting its files.

An unreadable or unparsable FILE is never merged over (rc 2): it may be an XFCE app's own file mid-write.
"""

import os
import sys
import xml.etree.ElementTree as ET

SCALARS = ("string", "bool", "int", "uint", "double")


class SpecError(ValueError):
    """A spec line that is not PATH<TAB>TYPE<TAB>VALUE."""


def parse_spec(text):
    """Return [(path_parts, type, value)] in spec order."""
    out = []
    for n, raw in enumerate(text.splitlines(), 1):
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        cols = raw.split("\t")
        if len(cols) == 2 and cols[1] == "replace":
            cols.append("")
        if len(cols) != 3:
            raise SpecError("spec line %d is not PATH<TAB>TYPE<TAB>VALUE: %r" % (n, raw))
        path, typ, value = cols
        parts = [p for p in path.split("/") if p]
        if not path.startswith("/") or not parts:
            raise SpecError("spec line %d: the path must start with / and name a property: %r" % (n, path))
        if typ != "replace" and typ not in SCALARS and not (
            typ.startswith("array:") and typ[len("array:"):] in SCALARS
        ):
            raise SpecError("spec line %d: unknown type %r" % (n, typ))
        out.append((parts, typ, value))
    return out


def _child(node, name):
    for c in node.findall("property"):
        if c.get("name") == name:
            return c
    return None


def _find(root, parts):
    node = root
    for p in parts:
        node = _child(node, p)
        if node is None:
            return None
    return node


def _ensure(root, parts):
    node = root
    for p in parts:
        nxt = _child(node, p)
        if nxt is None:
            nxt = ET.SubElement(node, "property", {"name": p, "type": "empty"})
        node = nxt
    return node


def _set(node, typ, value):
    for v in node.findall("value"):
        node.remove(v)
    if typ.startswith("array:"):
        et = typ[len("array:"):]
        node.attrib.pop("value", None)
        node.set("type", "array")
        for i, item in enumerate(value.split(",") if value else []):
            node.insert(i, ET.Element("value", {"type": et, "value": item}))
    else:
        node.set("type", typ)
        node.set("value", value)


def merge(channel, current_text, spec):
    """Return the merged channel XML text."""
    if current_text is None or not current_text.strip():
        root = ET.Element("channel", {"name": channel, "version": "1.0"})
    else:
        root = ET.fromstring(current_text)
        if root.tag != "channel" or root.get("name") != channel:
            raise SpecError("the file is not the xfconf channel %r" % channel)
    for parts, typ, _value in spec:
        if typ == "replace":
            node = _ensure(root, parts)
            for c in node.findall("property"):
                node.remove(c)
    for parts, typ, value in spec:
        if typ != "replace":
            _set(_ensure(root, parts), typ, value)
    for el in root.iter():
        el.text = None
        el.tail = None
    ET.indent(root, space="  ")
    return '<?xml version="1.0" encoding="UTF-8"?>\n\n' + ET.tostring(root, encoding="unicode") + "\n"


def stray_under_replace(paths, spec):
    """The first of PATHS that sits under a `replace` path of SPEC and is none of: a declared property, an
    ancestor of one (a container), a descendant of a declared non-replace property (an app's own detail,
    e.g. a panel plugin's settings). None when there is none. A replaced subtree holds only what the spec
    declares, so a property the spec dropped (a stale key binding, a removed plugin) is found here."""
    roots = ["/" + "/".join(parts) for parts, typ, _v in spec if typ == "replace"]
    declared = ["/" + "/".join(parts) for parts, typ, _v in spec if typ != "replace"]
    for path in paths:
        if not any(path.startswith(r + "/") for r in roots):
            continue
        if path in declared or any(d.startswith(path + "/") for d in declared):
            continue
        if any(path.startswith(d + "/") for d in declared):
            continue
        return path
    return None


def _file_paths(root, prefix=""):
    for node in root.findall("property"):
        path = prefix + "/" + node.get("name", "")
        yield path
        yield from _file_paths(node, path)


def check(current_text, spec):
    """Return None when every declared property holds and no replaced subtree carries anything else, else
    the first difference."""
    try:
        root = ET.fromstring(current_text)
    except ET.ParseError as exc:
        return "not a readable xfconf file (%s)" % exc
    stray = stray_under_replace(list(_file_paths(root)), spec)
    if stray:
        return "%s is not declared (a stale property under a replaced subtree)" % stray
    for parts, typ, value in spec:
        if typ == "replace":
            continue
        path = "/" + "/".join(parts)
        node = _find(root, parts)
        if node is None:
            return "%s missing" % path
        if typ.startswith("array:"):
            et = typ[len("array:"):]
            got = [(v.get("type"), v.get("value")) for v in node.findall("value")]
            want = [(et, item) for item in (value.split(",") if value else [])]
            if node.get("type") != "array" or got != want:
                return "%s = %s (want %s)" % (path, [g[1] for g in got], [w[1] for w in want])
        elif node.get("type") != typ or node.get("value") != value:
            return "%s = %s %r (want %s %r)" % (path, node.get("type"), node.get("value"), typ, value)
    return None


def _read(path):
    if path == "-":
        return sys.stdin.read()
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except FileNotFoundError:
        return None


def query_args(channel, spec):
    """The xfconf-query argv lists that apply SPEC to a RUNNING xfconfd, in spec order: every `replace`
    first (a recursive reset), then each property created or set with its type."""
    out = []
    for parts, typ, _value in spec:
        if typ == "replace":
            out.append(["xfconf-query", "-c", channel, "-p", "/" + "/".join(parts), "-r", "-R"])
    for parts, typ, value in spec:
        if typ == "replace":
            continue
        args = ["xfconf-query", "-c", channel, "-p", "/" + "/".join(parts), "-n"]
        if typ.startswith("array:"):
            args.append("-a")
            for item in value.split(",") if value else []:
                args += ["-t", typ[len("array:"):], "-s", item]
        else:
            args += ["-t", typ, "-s", value]
        out.append(args)
    return out


def parse_query_value(text):
    """xfconf-query -p P output -> a scalar string, or a list for an array."""
    lines = text.splitlines()
    if lines and lines[0].startswith("Value is an array with"):
        return [ln for ln in lines[1:] if ln.strip()]
    return text.rstrip("\n")


def _run(argv):
    import subprocess
    try:
        return subprocess.run(argv, capture_output=True, text=True, timeout=20)
    except subprocess.TimeoutExpired:
        return subprocess.CompletedProcess(argv, 124, "", "timed out after 20 s (no xfconfd answer)")
    except OSError as exc:
        return subprocess.CompletedProcess(argv, 127, "", str(exc))


def verify_live(channel, spec, run=_run):
    """Read every declared property back from the running xfconfd, and its property list for a stale one
    under a replaced subtree. Returns None or the first difference."""
    if any(typ == "replace" for _p, typ, _v in spec):
        r = run(["xfconf-query", "-c", channel, "-l"])
        if r.returncode != 0:
            return "the property list read failed (rc %s): %s" % (r.returncode, (r.stderr or r.stdout).strip())
        stray = stray_under_replace([ln.strip() for ln in r.stdout.splitlines() if ln.strip()], spec)
        if stray:
            return "%s is not declared (a stale property under a replaced subtree)" % stray
    for parts, typ, value in spec:
        if typ == "replace":
            continue
        path = "/" + "/".join(parts)
        r = run(["xfconf-query", "-c", channel, "-p", path])
        if r.returncode != 0:
            return "%s: read-back failed (rc %s): %s" % (path, r.returncode, (r.stderr or r.stdout).strip())
        got = parse_query_value(r.stdout)
        want = (value.split(",") if value else []) if typ.startswith("array:") else value
        if got != want:
            return "%s read back %r (want %r)" % (path, got, want)
    return None


def apply_live(channel, spec, run=_run):
    """Apply SPEC through a running xfconfd (the caller runs this as the desktop user on its session bus),
    then read every declared property back. Returns None or the first failure."""
    for argv in query_args(channel, spec):
        r = run(argv)
        # a reset of a property that does not exist yet fails harmlessly; a set must succeed
        if r.returncode != 0 and argv[-2:] != ["-r", "-R"]:
            return "%s failed (rc %s): %s" % (" ".join(argv), r.returncode, (r.stderr or r.stdout).strip())
    return verify_live(channel, spec, run)


def main(argv):
    if len(argv) == 4 and argv[1] in ("apply", "verify-live"):
        channel, spec_path = argv[2], argv[3]
        try:
            spec = parse_spec(_read(spec_path) or "")
        except (SpecError, OSError, UnicodeDecodeError) as exc:
            print("obs_box_xfconf: %s -- nothing applied" % exc, file=sys.stderr)
            return 2
        why = (apply_live if argv[1] == "apply" else verify_live)(channel, spec)
        if why:
            print("obs_box_xfconf: %s: %s" % (channel, why), file=sys.stderr)
            return 1
        print("ok")
        return 0
    if len(argv) == 5 and argv[1] == "merge":
        channel, path, spec_path = argv[2], argv[3], argv[4]
        try:
            spec = parse_spec(_read(spec_path) or "")
            current = _read(path)
            sys.stdout.write(merge(channel, current, spec))
        except (SpecError, ET.ParseError, OSError, UnicodeDecodeError) as exc:
            print("obs_box_xfconf: %s: %s -- refusing to merge" % (path, exc), file=sys.stderr)
            return 2
        return 0
    if len(argv) == 4 and argv[1] == "check":
        path, spec_path = argv[2], argv[3]
        try:
            spec = parse_spec(_read(spec_path) or "")
            current = _read(path)
        except (SpecError, OSError, UnicodeDecodeError) as exc:
            print("%s: %s" % (path, exc))
            return 1
        if current is None:
            print("%s missing" % path)
            return 1
        why = check(current, spec)
        if why:
            print("%s: %s" % (os.path.basename(path), why))
            return 1
        print("ok")
        return 0
    print(__doc__.strip().splitlines()[0], file=sys.stderr)
    print("usage: obs_box_xfconf.py merge CHANNEL FILE SPEC | check FILE SPEC | apply|verify-live CHANNEL SPEC",
          file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
