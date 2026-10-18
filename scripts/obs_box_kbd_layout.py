#!/usr/bin/env python3
"""The keyboard layout item of the Linux OBS boxes' panel (issue 1441).

Owner 18.10.2026: "a na stream notebookoch uz je slovenska klavesnica?? da sa v liste prepinat??"
The boxes carry two XKB layouts (us,sk with Alt+Shift, setup-strih step 16g), but the tint2 panel
showed neither which one is active nor a way to switch it with the mouse. tint2 runs this file as an
EXECUTOR item:

    watch  prints the active layout (US / SK) once and again on every change (the continuous executor)
    next   locks the next layout (the item's left click)
    print  prints the active layout once (verify-strih item 44, row panel-kbd)

Ubuntu 26.04 ships no xkb-switch / xkblayout-state, so the group is read and set through libX11's own
XKB calls over ctypes: XkbGetState (the locked group) and XkbLockGroup. The layout NAMES come from
`setxkbmap -query` (`layout: us,sk`). Exit 1 + one stderr line when X does not answer.
"""

import ctypes
import ctypes.util
import subprocess
import sys
import time

XKB_USE_CORE_KBD = 0x0100
WATCH_POLL_S = 0.25
WATCH_LAYOUT_REREAD_S = 10.0
WATCH_RECONNECT_S = 2.0


def parse_layouts(query_text):
    """`setxkbmap -query` text -> the layout names in group order (`["us", "sk"]`); [] when absent."""
    for line in query_text.splitlines():
        key, sep, value = line.partition(":")
        if sep and key.strip() == "layout":
            return [name.strip() for name in value.split(",") if name.strip()]
    return []


def label_for(group, layouts):
    """The panel text for GROUP: the layout name upper-cased without a variant (`us(intl)` -> `US`);
    `?` when the group has no name (no layout read, or a group past the list)."""
    if 0 <= group < len(layouts):
        return layouts[group].split("(", 1)[0].upper() or "?"
    return "?"


def next_group(group, count):
    """The group a click locks: the next one, wrapping; 0 when no layout is known."""
    if count <= 0:
        return 0
    return (group + 1) % count


class XkbStateRec(ctypes.Structure):
    """XKBstr.h `XkbStateRec`."""

    _fields_ = [
        ("group", ctypes.c_ubyte),
        ("locked_group", ctypes.c_ubyte),
        ("base_group", ctypes.c_ushort),
        ("latched_group", ctypes.c_ushort),
        ("mods", ctypes.c_ubyte),
        ("base_mods", ctypes.c_ubyte),
        ("latched_mods", ctypes.c_ubyte),
        ("locked_mods", ctypes.c_ubyte),
        ("compat_state", ctypes.c_ubyte),
        ("grab_mods", ctypes.c_ubyte),
        ("compat_grab_mods", ctypes.c_ubyte),
        ("lookup_mods", ctypes.c_ubyte),
        ("compat_lookup_mods", ctypes.c_ubyte),
        ("ptr_buttons", ctypes.c_ushort),
    ]


class X11Keyboard:
    """The thin libX11 seam: open :DISPLAY, read the locked group, lock another one."""

    def __init__(self):
        name = ctypes.util.find_library("X11")
        if not name:
            raise OSError("libX11 not found")
        self.lib = ctypes.CDLL(name)
        self.lib.XOpenDisplay.restype = ctypes.c_void_p
        self.lib.XOpenDisplay.argtypes = [ctypes.c_char_p]
        self.lib.XCloseDisplay.argtypes = [ctypes.c_void_p]
        self.lib.XkbGetState.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.POINTER(XkbStateRec)]
        self.lib.XkbGetState.restype = ctypes.c_int
        self.lib.XkbLockGroup.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_uint]
        self.lib.XkbLockGroup.restype = ctypes.c_int
        self.lib.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
        self.display = self.lib.XOpenDisplay(None)
        if not self.display:
            raise OSError("cannot open the X display (DISPLAY not set or X down)")

    def group(self):
        state = XkbStateRec()
        if self.lib.XkbGetState(self.display, XKB_USE_CORE_KBD, ctypes.byref(state)) != 0:
            raise OSError("XkbGetState failed")
        return int(state.group)

    def lock(self, group):
        if not self.lib.XkbLockGroup(self.display, XKB_USE_CORE_KBD, group):
            raise OSError("XkbLockGroup failed")
        self.lib.XSync(self.display, 0)

    def close(self):
        if self.display:
            self.lib.XCloseDisplay(self.display)
            self.display = None


def read_layouts():
    """The layout names of the running X server; [] when setxkbmap fails."""
    try:
        out = subprocess.run(["setxkbmap", "-query"], capture_output=True, text=True, timeout=5, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        print("obs_box_kbd_layout: setxkbmap -query failed: %s" % exc, file=sys.stderr)
        return []
    if out.returncode != 0:
        print("obs_box_kbd_layout: setxkbmap -query exit %d: %s" % (out.returncode, out.stderr.strip()),
              file=sys.stderr)
        return []
    return parse_layouts(out.stdout)


def cmd_print(keyboard_factory=X11Keyboard, layouts_reader=read_layouts):
    kbd = keyboard_factory()
    try:
        print(label_for(kbd.group(), layouts_reader()), flush=True)
    finally:
        kbd.close()
    return 0


def cmd_next(keyboard_factory=X11Keyboard, layouts_reader=read_layouts):
    kbd = keyboard_factory()
    try:
        layouts = layouts_reader()
        current = kbd.group()
        target = next_group(current, len(layouts))
        kbd.lock(target)
        print("obs_box_kbd_layout: layout %s -> %s" % (label_for(current, layouts), label_for(target, layouts)),
              file=sys.stderr)
    finally:
        kbd.close()
    return 0


def cmd_watch(keyboard_factory=X11Keyboard, layouts_reader=read_layouts, clock=time.monotonic,
              sleep=time.sleep, max_iterations=None):
    """Print the label at start and on every change. A lost X connection prints `?` once and reconnects
    every WATCH_RECONNECT_S; MAX_ITERATIONS bounds the loop for the tests (None = forever)."""
    shown = None
    kbd = None
    layouts, layouts_at = [], None
    n = 0
    while max_iterations is None or n < max_iterations:
        n += 1
        try:
            if kbd is None:
                kbd = keyboard_factory()
            now = clock()
            if layouts_at is None or now - layouts_at >= WATCH_LAYOUT_REREAD_S:
                layouts, layouts_at = layouts_reader(), now
            label = label_for(kbd.group(), layouts)
        except OSError as exc:
            if kbd is not None:
                kbd.close()
                kbd = None
            if shown != "?":
                print("obs_box_kbd_layout: %s" % exc, file=sys.stderr)
            label = "?"
            if shown != label:
                print(label, flush=True)
                shown = label
            sleep(WATCH_RECONNECT_S)
            continue
        if label != shown:
            print(label, flush=True)
            shown = label
        sleep(WATCH_POLL_S)
    if kbd is not None:
        kbd.close()
    return 0


def main(argv):
    if len(argv) != 2 or argv[1] not in ("watch", "next", "print"):
        print("usage: obs_box_kbd_layout.py watch|next|print", file=sys.stderr)
        return 2
    try:
        return {"watch": cmd_watch, "next": cmd_next, "print": cmd_print}[argv[1]]()
    except OSError as exc:
        print("obs_box_kbd_layout: %s" % exc, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
