#!/usr/bin/env python3
"""issue 1470 -- read and seed one plugin dock in an OBS user.ini [BasicWindow] DockState.

OBS hides a dock a plugin adds: `obs_frontend_add_dock_by_id` ends with `setVisible(false)` +
`setFloating(true)` (vendor/obs-studio/frontend/OBSStudioAPI.cpp), and the saved DockState, restored
after the plugins load (`OBSBasic.cpp` `restoreState`), decides where it goes. A box whose saved layout
never held the dock (stream-pp, 23.10.2026) therefore loads it invisible; stream-snv's layout, ported
from the Windows stream PC, already holds it docked and visible.

DockState is base64 of `QMainWindow::saveState()`. This module parses the dock-area part of that blob
(the Qt 6 `QDockAreaLayout` / `QDockAreaLayoutInfo` stream format) and seeds ONE dock, visible, docked:
- no entry of that object name at all -> a new item at the end of the right dock area (the area is
  created when the state has none);
- an entry in OBS's own add-time default (hidden AND floating: what an OBS that ran with the plugin
  before any seed saves on exit) -> that entry is re-docked visible in place, same byte length -- but
  ONLY before the first seed: OBS saves the same state when the operator floats the dock and closes it,
  so the first seed decision writes `[CameraBox] AvsyncDockSeeded=true` into the same user.ini (the same
  atomic rewrite), and once it is there a hidden + floating entry is the operator's. A replaced or
  restored user.ini loses the marker together with the layout, so its next seed re-docks the dock;
- any other entry (visible, or closed while docked) is the operator's choice and is kept.
Anything the parser does not recognise is refused: the blob is never guessed at. A dock added to a
TABBED right area becomes a tab behind the current one (visible in the state, not on top): Qt's own
behaviour, never seen in a captured layout.

  python3 scripts/obs_dock_state.py check --ini ~/.config/obs-studio/user.ini --dock <object name>
  python3 scripts/obs_dock_state.py seed  --ini ~/.config/obs-studio/user.ini --dock <object name>

`check` prints one token: visible | hidden | default-hidden | absent | no-state | no-ini | unreadable
(hidden = closed by the operator; default-hidden = OBS's hidden + floating default, before the first seed).
`seed` prints seeded | redocked | present | no-state | no-ini, exit 0; exit 3 = an unreadable user.ini or
DockState (nothing written); exit 4 = a symlinked user.ini (nothing written). Run `seed` only while OBS
is stopped: OBS rewrites user.ini from memory on exit.
"""
import argparse
import base64
import binascii
import os
import re
import struct
import sys
import tempfile

# Qt's markers (qdockarealayout_p.h / qmainwindowlayout_p.h).
VERSION_MARKER = 0xFF
DOCK_STATE_MARKER = 0xFD
SEQUENCE_MARKER = 0xFC
WIDGET_MARKER = 0xFB
TAB_MARKER = 0xFA
FLOATING_TAB_MARKER = 0xF9
STATE_FLAG_VISIBLE = 0x01
STATE_FLAG_FLOATING = 0x02
# QInternal::DockPosition: the four dock areas, the index Qt writes before each area.
DOCK_LEFT, DOCK_RIGHT, DOCK_TOP, DOCK_BOTTOM = 0, 1, 2, 3
# Qt::Orientation of a right / left area's top-level sequence.
QT_VERTICAL = 2
# Qt::DockWidgetArea values a corner may hold.
CORNER_AREAS = (1, 2, 4, 8)
# The seeded right area's size: the stream PC's own (stream-snv's ported layout, 352 x 431).
SEED_AREA_WIDTH = 352
SEED_AREA_HEIGHT = 431


class DockStateError(ValueError):
    """The blob is not a QMainWindow dock state this parser understands."""


class _Reader:
    def __init__(self, blob):
        self.b = blob
        self.p = 0

    def need(self, n):
        if self.p + n > len(self.b):
            raise DockStateError("truncated at byte %d (need %d more)" % (self.p, n))

    def u8(self):
        self.need(1)
        v = self.b[self.p]
        self.p += 1
        return v

    def i32(self):
        self.need(4)
        (v,) = struct.unpack(">i", self.b[self.p:self.p + 4])
        self.p += 4
        return v

    def qstring(self):
        self.need(4)
        (n,) = struct.unpack(">I", self.b[self.p:self.p + 4])
        self.p += 4
        if n == 0xFFFFFFFF:
            return None
        if n % 2:
            raise DockStateError("odd QString byte length %d at byte %d" % (n, self.p - 4))
        self.need(n)
        s = self.b[self.p:self.p + n].decode("utf-16-be")
        self.p += n
        return s


def _parse_info(r, items, in_area=True):
    """One QDockAreaLayoutInfo. Appends every widget item {name, flags, flags_offset, in_area} to
    `items` (in_area False = an item of a floating tab group).
    Returns (start, count_offset, end): the info's first byte, the offset of its item count and the
    offset just past its last item."""
    start = r.p
    marker = r.u8()
    if marker == TAB_MARKER:
        r.i32()  # the current tab index
    elif marker != SEQUENCE_MARKER:
        raise DockStateError("dock info marker 0x%02x at byte %d" % (marker, start))
    r.u8()  # orientation
    count_offset = r.p
    count = r.i32()
    if count < 0 or count > 1000:
        raise DockStateError("dock item count %d at byte %d" % (count, count_offset))
    for _ in range(count):
        at = r.p
        m = r.u8()
        if m == WIDGET_MARKER:
            name = r.qstring()
            flags_offset = r.p
            flags = r.u8()
            for _ in range(4):  # pos, size, 2 dummies -- or the floating geometry x, y, w, h
                r.i32()
            items.append({"name": name, "flags": flags, "flags_offset": flags_offset, "in_area": in_area})
        elif m == SEQUENCE_MARKER:
            for _ in range(4):  # the nested info's pos, size, 2 dummies
                r.i32()
            _parse_info(r, items, in_area)
        else:
            raise DockStateError("dock item marker 0x%02x at byte %d" % (m, at))
    return start, count_offset, r.p


def parse(blob):
    """The dock part of a QMainWindow::saveState blob: {"areas": [{"pos", "width", "height",
    "start", "info_count_offset", "info_end"}], "areas_count_offset", "areas_end", "items"}.
    Raises DockStateError on anything else."""
    r = _Reader(blob)
    if r.i32() != VERSION_MARKER:
        raise DockStateError("not a QMainWindow state (no version marker)")
    r.i32()  # the caller's version number
    if r.u8() != DOCK_STATE_MARKER:
        raise DockStateError("no dock-widget state in the blob")
    areas_count_offset = r.p
    n = r.i32()
    if n < 0 or n > 4:
        raise DockStateError("dock area count %d" % n)
    areas, items, seen = [], [], set()
    for _ in range(n):
        start = r.p
        pos = r.i32()
        if pos not in (DOCK_LEFT, DOCK_RIGHT, DOCK_TOP, DOCK_BOTTOM) or pos in seen:
            raise DockStateError("dock area position %d at byte %d" % (pos, start))
        seen.add(pos)
        width, height = r.i32(), r.i32()
        _, count_offset, end = _parse_info(r, items)
        areas.append({"pos": pos, "width": width, "height": height, "start": start,
                      "info_count_offset": count_offset, "info_end": end})
    areas_end = r.p
    r.i32()
    r.i32()  # the central widget size
    for _ in range(4):
        corner = r.i32()
        if corner not in CORNER_AREAS:
            raise DockStateError("corner value %d" % corner)
    # Floating tabbed groups (Qt writes them after the dock areas): a QRect, then one info each.
    while r.p < len(blob) and blob[r.p] == FLOATING_TAB_MARKER:
        r.u8()
        for _ in range(4):
            r.i32()
        _parse_info(r, items, False)
    return {"areas": areas, "areas_count_offset": areas_count_offset, "areas_end": areas_end,
            "items": items}


def _qstring_bytes(name):
    data = name.encode("utf-16-be")
    return struct.pack(">I", len(data)) + data


def mentions(blob, name):
    """True when the blob carries `name` as a QString anywhere (an operator entry of any kind)."""
    return _qstring_bytes(name) in blob


def _find(blob, name):
    """(parsed state, the one item named `name` or None). DockStateError on two entries, or on a name
    the blob carries outside every parsed item."""
    st = parse(blob)
    hits = [i for i in st["items"] if i["name"] == name]
    if len(hits) > 1:
        raise DockStateError("%d entries named %s" % (len(hits), name))
    if not hits and mentions(blob, name):
        raise DockStateError("%s is in the blob outside the parsed dock items" % name)
    return st, (hits[0] if hits else None)


def _item_state(item):
    if item is None:
        return "absent"
    if item["flags"] & STATE_FLAG_VISIBLE:
        return "visible"
    if item["flags"] == STATE_FLAG_FLOATING and item["in_area"]:
        return "default-hidden"
    return "hidden"


def dock_state(blob, name):
    """visible | hidden | default-hidden | absent for `name` (DockStateError on an unreadable blob).
    default-hidden = hidden AND floating in a dock area: what OBS's own add leaves (OBSStudioAPI.cpp)."""
    return _item_state(_find(blob, name)[1])


def seed_action(blob, name, seeded_before=False):
    """insert | redock | keep: what `seed` does to `name` in this blob (DockStateError: unreadable).
    seeded_before: a first seed already ran, so OBS's hidden + floating default is the operator's."""
    state = dock_state(blob, name)
    if state == "default-hidden" and seeded_before:
        return "keep"
    return {"absent": "insert", "default-hidden": "redock"}.get(state, "keep")


def _widget_item(name):
    # A docked, visible widget: marker, name, flags, pos 0, size -1 (Qt sizes it), two dummies.
    return (bytes([WIDGET_MARKER]) + _qstring_bytes(name) + bytes([STATE_FLAG_VISIBLE])
            + struct.pack(">iiii", 0, -1, 0, 0))


def seed(blob, name, width=SEED_AREA_WIDTH, height=SEED_AREA_HEIGHT):
    """The blob with `name` docked visible: inserted at the end of the right dock area when absent,
    re-docked in place when in OBS's hidden + floating default. Raises DockStateError when the blob is
    unreadable or the entry is the operator's own (visible, or closed while docked): never replaced."""
    st, found = _find(blob, name)
    if found is not None:
        if _item_state(found) != "default-hidden":
            raise DockStateError("%s is in the dock state as the operator left it -- kept" % name)
        # Same length: the flags byte, then pos 0, size -1 (Qt sizes it) and the two dummies in place
        # of the floating geometry. The item stays in the dock area OBS added it to.
        at = found["flags_offset"]
        out = bytearray(blob)
        out[at:at + 17] = bytes([STATE_FLAG_VISIBLE]) + struct.pack(">iiii", 0, -1, 0, 0)
        return bytes(out)
    item = _widget_item(name)
    right = [a for a in st["areas"] if a["pos"] == DOCK_RIGHT]
    if right:
        a = right[0]
        (count,) = struct.unpack(">i", blob[a["info_count_offset"]:a["info_count_offset"] + 4])
        out = bytearray(blob)
        out[a["info_end"]:a["info_end"]] = item
        out[a["info_count_offset"]:a["info_count_offset"] + 4] = struct.pack(">i", count + 1)
        return bytes(out)
    # A new right area, written where Qt writes it: before the first area of a higher position.
    area = (struct.pack(">iii", DOCK_RIGHT, width, height) + bytes([SEQUENCE_MARKER, QT_VERTICAL])
            + struct.pack(">i", 1) + item)
    at = st["areas_end"]
    for a in st["areas"]:
        if a["pos"] > DOCK_RIGHT:
            at = a["start"]
            break
    out = bytearray(blob)
    out[at:at] = area
    off = st["areas_count_offset"]
    out[off:off + 4] = struct.pack(">i", len(st["areas"]) + 1)
    return bytes(out)


# --- user.ini -------------------------------------------------------------------------------------

_SECTION_RE = re.compile(r"^\s*\[(?P<name>[^\]]*)\]\s*$")


def _dockstate_line(lines):
    """(index, value) of the one [BasicWindow] DockState= line; (None, None) when absent. Raises
    DockStateError on two of them (OBS would read one of them, we never guess which)."""
    section, found = None, []
    for i, line in enumerate(lines):
        m = _SECTION_RE.match(line.rstrip("\r\n"))
        if m:
            section = m.group("name")
            continue
        if section == "BasicWindow" and line.startswith("DockState="):
            found.append((i, line[len("DockState="):].rstrip("\r\n")))
    if len(found) > 1:
        raise DockStateError("%d DockState lines in [BasicWindow]" % len(found))
    return found[0] if found else (None, None)


def _decode(value):
    try:
        return base64.b64decode(value.strip(), validate=True)
    except (binascii.Error, ValueError) as e:
        raise DockStateError("DockState is not base64 (%s)" % e)


def _read_lines(path):
    # newline="": the lines keep their own endings, so a CRLF user.ini is written back CRLF.
    with open(path, encoding="utf-8", newline="") as fh:
        return fh.readlines()


# The first-seed marker: a key of OUR section in the same user.ini, so a replaced or restored user.ini
# loses it together with the layout. OBS's config save keeps a section it does not know.
MARKER_SECTION = "CameraBox"
MARKER_KEY = "AvsyncDockSeeded"


def _section_of(lines):
    """[(section name or None)] per line."""
    out, section = [], None
    for line in lines:
        m = _SECTION_RE.match(line.rstrip("\r\n"))
        if m:
            section = m.group("name")
        out.append(section)
    return out


def _marked(lines):
    sections = _section_of(lines)
    for line, section in zip(lines, sections):
        if section == MARKER_SECTION and line.rstrip("\r\n").strip() == MARKER_KEY + "=true":
            return True
    return False


def _with_marker(lines, ending):
    """LINES with `[CameraBox] AvsyncDockSeeded=true`: an existing key line of the section is replaced
    (never a second copy: libobs and configparser read duplicates differently), else the key goes in
    right after the section header, else a new section at the end; every new line ends ENDING."""
    sections = _section_of(lines)
    for i, (line, section) in enumerate(zip(lines, sections)):
        if section == MARKER_SECTION and line.split("=", 1)[0].strip() == MARKER_KEY:
            return lines[:i] + [MARKER_KEY + "=true" + (line[len(line.rstrip("\r\n")):] or ending)] + lines[i + 1:]
    for i, (line, section) in enumerate(zip(lines, sections)):
        if section == MARKER_SECTION and _SECTION_RE.match(line.rstrip("\r\n")):
            return lines[:i + 1] + [MARKER_KEY + "=true" + ending] + lines[i + 1:]
    out = list(lines)
    if out and not out[-1].endswith(("\n", "\r")):
        out[-1] += ending
    return out + [ending, "[" + MARKER_SECTION + "]" + ending, MARKER_KEY + "=true" + ending]


def check_ini(path, name):
    if not os.path.exists(path):
        return "no-ini"
    try:
        lines = _read_lines(path)
        _, value = _dockstate_line(lines)
        if value is None:
            return "no-state"
        state = dock_state(_decode(value), name)
    except (DockStateError, OSError, UnicodeDecodeError):
        return "unreadable"
    if state == "default-hidden" and _marked(lines):
        return "hidden"
    return state


def _replace(path, lines):
    """Write LINES over PATH: a temp file in the same directory renamed over the name (a reader never sees
    half a file; a link is replaced, never written through), the mode kept, and as root the file's owner
    kept too (setup-strih runs as root; OBS must still save the desktop user's config)."""
    st = os.stat(path)
    fd, tmp = tempfile.mkstemp(prefix=".user.ini.", dir=os.path.dirname(os.path.abspath(path)))
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as fh:
            fh.writelines(lines)
        os.chmod(tmp, st.st_mode & 0o777)
        if os.geteuid() == 0:
            os.chown(tmp, st.st_uid, st.st_gid)
        os.replace(tmp, path)
    except BaseException:
        os.unlink(tmp)
        raise


def seed_ini(path, name):
    """seeded | redocked | present | no-state | no-ini. Raises DockStateError (unreadable) or PermissionError
    (a symlinked user.ini). Every seed decision (seeded / redocked / present) leaves the first-seed marker
    in user.ini; once it is there, OBS's hidden + floating default is kept as the operator's."""
    if os.path.islink(path):
        raise PermissionError("%s is a symlink -- refusing to write it" % path)
    if not os.path.exists(path):
        return "no-ini"
    try:
        lines = _read_lines(path)
    except UnicodeDecodeError as e:
        raise DockStateError("user.ini is not UTF-8 (%s)" % e)
    idx, value = _dockstate_line(lines)
    if value is None:
        return "no-state"
    blob = _decode(value)
    marked = _marked(lines)
    action = seed_action(blob, name, marked)
    ending = lines[idx][len(lines[idx].rstrip("\r\n")):] or "\n"
    if action != "keep":
        lines[idx] = "DockState=" + base64.b64encode(seed(blob, name)).decode("ascii") + ending
    if action == "keep" and marked:
        return "present"
    _replace(path, lines if marked else _with_marker(lines, ending))
    return {"insert": "seeded", "redock": "redocked"}.get(action, "present")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("action", choices=("check", "seed"))
    ap.add_argument("--ini", required=True, help="the OBS user.ini")
    ap.add_argument("--dock", required=True, help="the dock's object name")
    a = ap.parse_args(argv)
    if a.action == "check":
        print(check_ini(a.ini, a.dock))
        return 0
    try:
        print(seed_ini(a.ini, a.dock))
        return 0
    except PermissionError as e:
        print("refused: %s" % e, file=sys.stderr)
        return 4
    except (DockStateError, OSError) as e:
        print("unreadable: %s -- left untouched" % e, file=sys.stderr)
        return 3


if __name__ == "__main__":
    sys.exit(main())
