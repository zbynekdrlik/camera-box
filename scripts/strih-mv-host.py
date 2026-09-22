#!/usr/bin/env python3
"""strih-mv-host.py -- host OBS projector windows inside a plain managed X11 toplevel.

WHY (strih-lx, 22.9.2026): under XWayland + NVIDIA PRIME render offload an OBS projector whose GL
surface IS the X toplevel (OBSProjector = OBSQTDisplay(widget, Qt::Window)) stalls the graphics
thread ~0.5 s per present (program render lag 93 %, multiview 1.8 fps), while a GL surface that is
a CHILD window presents fine (the main window's preview/program displays). Reparenting the projector
under the OBS main window by hand gave lag 93.5 % -> 0.0 %, multiview 1.8 -> 29.8 fps. This helper
does that structurally: for every OBS projector toplevel it creates a plain host toplevel (managed
by the WM like any window: move / resize / fullscreen), reparents the projector's X window into it,
keeps the child sized to the host, forwards focus and the WM close to the child. When OBS destroys
the projector the host is destroyed too. Runs forever, polling every second; idempotent.

Fail-loud: any X error outside the per-window handlers terminates the process (systemd restarts
it); per-window X errors (a window vanishing mid-operation is normal) are LOGGED, never silent.
Nothing here touches OBS state: the projector keeps its own OBS-side lifecycle (Escape /
click-to-switch still reach it as X events go to the child).
"""
import os
import time

from Xlib import X, Xatom, display
from Xlib.error import XError
from Xlib.protocol import event as xev

POLL_S = 1.0
TITLE_KEYS = ("Projector",)  # OBS titles: "Projector - Multiview", "Windowed Projector (...)", ...
HOST_CLASS = "obs-projector-host"


def log(msg: str) -> None:
    print(f"strih-mv-host: {msg}", flush=True)


def wm_title(win) -> str:
    try:
        p = win.get_full_property(NET_WM_NAME, 0)
        if p and p.value:
            v = p.value
            return v.decode("utf-8", "replace") if isinstance(v, bytes) else str(v)
        n = win.get_wm_name()
        return n if isinstance(n, str) else (n.decode("utf-8", "replace") if n else "")
    except XError as e:
        log(f"wm_title 0x{win.id:x}: X error (window gone?): {e}")
        return ""


def wm_class(win) -> str:
    try:
        c = win.get_wm_class()
        return c[1] if c else ""
    except XError as e:
        log(f"wm_class 0x{win.id:x}: X error (window gone?): {e}")
        return ""


def client_list():
    p = root.get_full_property(NET_CLIENT_LIST, Xatom.WINDOW)
    return list(p.value) if p and p.value else []


def geometry_abs(win):
    g = win.get_geometry()
    tr = root.translate_coords(win, 0, 0)  # child coords -> root coords (works through WM frames)
    return -tr.x, -tr.y, g.width, g.height


def make_host(title: str, x: int, y: int, w: int, h: int):
    host = root.create_window(
        max(x, 0), max(y, 0), max(w, 64), max(h, 64), 0, scr.root_depth, X.InputOutput, X.CopyFromParent,
        background_pixel=scr.black_pixel,
        event_mask=X.StructureNotifyMask | X.SubstructureNotifyMask | X.FocusChangeMask,
    )
    host.set_wm_name(title)
    host.change_property(NET_WM_NAME, UTF8_STRING, 8, title.encode("utf-8"))
    host.set_wm_class(HOST_CLASS, HOST_CLASS)
    host.set_wm_protocols([WM_DELETE_WINDOW, WM_TAKE_FOCUS])
    host.set_wm_normal_hints(flags=0)
    host.map()
    d.sync()
    return host


hosts = {}    # host window id -> projector window
by_proj = {}  # projector window id -> host window


def adopt(proj_id: int) -> None:
    proj = d.create_resource_object("window", proj_id)
    title = wm_title(proj)
    x, y, w, h = geometry_abs(proj)
    host = make_host(title, x, y, w, h)
    proj.change_attributes(event_mask=X.StructureNotifyMask)
    proj.reparent(host, 0, 0)
    proj.configure(x=0, y=0, width=max(w, 64), height=max(h, 64))
    proj.map()
    d.sync()
    hosts[host.id] = proj
    by_proj[proj_id] = host
    log(f"hosted projector 0x{proj_id:x} {title!r} in host 0x{host.id:x} at {x},{y} {w}x{h}")


def readopt_orphan_host(host) -> None:
    """A host left behind by a previous helper instance (RetainPermanent keeps it alive across our
    own crash/restart so X never destroys the projector child with it): take it over again."""
    try:
        for child in host.query_tree().children:
            if any(k in wm_title(child) for k in TITLE_KEYS):
                hosts[host.id] = child
                by_proj[child.id] = host
                host.change_attributes(event_mask=X.StructureNotifyMask | X.SubstructureNotifyMask | X.FocusChangeMask)
                child.change_attributes(event_mask=X.StructureNotifyMask)
                log(f"re-adopted orphan host 0x{host.id:x} with projector 0x{child.id:x}")
                return
        # An orphan host with NO projector inside (the X server handed the projector back to
        # root when the previous instance died) is just an empty window the operator would see
        # as a duplicate "Projector" entry -- destroy it; the projector itself gets a fresh host.
        host.destroy()
        d.sync()
        log(f"destroyed empty orphan host 0x{host.id:x}")
    except XError as e:
        log(f"re-adopt of orphan host 0x{host.id:x} failed: {e}")


def release_all() -> None:
    """Clean stop: give every projector back to the root window BEFORE our hosts go away, so a
    helper restart never takes an OBS projector down with it."""
    for host_id, proj in list(hosts.items()):
        try:
            x, y, w, h = geometry_abs(proj)
            proj.reparent(root, max(x, 0), max(y, 0))
            d.create_resource_object("window", host_id).destroy()
            log(f"released projector 0x{proj.id:x} to root, host 0x{host_id:x} destroyed")
        except XError as e:
            log(f"release of projector 0x{proj.id:x} failed: {e}")
    hosts.clear()
    by_proj.clear()
    d.sync()


def scan() -> None:
    for wid in client_list():
        if wid in by_proj or wid in hosts:
            continue
        win = d.create_resource_object("window", wid)
        if wm_class(win) == HOST_CLASS:
            readopt_orphan_host(win)
            continue
        title = wm_title(win)
        if not any(k in title for k in TITLE_KEYS):
            continue
        try:
            adopt(wid)
        except XError as e:
            log(f"adopt 0x{wid:x} {title!r} failed (will retry next scan): {e}")


def focus_child(host_id: int) -> None:
    try:
        hosts[host_id].set_input_focus(X.RevertToParent, X.CurrentTime)
    except XError as e:
        log(f"focus child of host 0x{host_id:x} failed: {e}")


def handle(ev) -> None:
    if ev.type == X.ConfigureNotify and ev.window.id in hosts:
        proj = hosts[ev.window.id]
        try:
            proj.configure(x=0, y=0, width=max(ev.width, 64), height=max(ev.height, 64))
        except XError as e:
            log(f"resize child 0x{proj.id:x} to {ev.width}x{ev.height} failed: {e}")
    elif ev.type == X.DestroyNotify:
        if ev.window.id in by_proj:  # OBS closed the projector -> drop the host
            host = by_proj.pop(ev.window.id)
            hosts.pop(host.id, None)
            try:
                host.destroy()
            except XError as e:
                log(f"destroy host 0x{host.id:x} failed (already gone?): {e}")
            log(f"projector 0x{ev.window.id:x} gone; host 0x{host.id:x} destroyed")
        elif ev.window.id in hosts:
            proj = hosts.pop(ev.window.id)
            by_proj.pop(proj.id, None)
            log(f"host 0x{ev.window.id:x} destroyed externally; projector 0x{proj.id:x} released")
    elif ev.type == X.FocusIn and ev.window.id in hosts:
        focus_child(ev.window.id)
    elif ev.type == X.ClientMessage and ev.window.id in hosts and ev.client_type == WM_PROTOCOLS:
        if ev.data[1][0] == WM_DELETE_WINDOW:
            proj = hosts[ev.window.id]
            try:  # ask OBS to close the projector (it saves its geometry + tears down the display)
                proj.send_event(xev.ClientMessage(window=proj, client_type=WM_PROTOCOLS,
                                                  data=(32, [WM_DELETE_WINDOW, X.CurrentTime, 0, 0, 0])))
                d.sync()
                log(f"host 0x{ev.window.id:x} close requested -> forwarded WM_DELETE_WINDOW to projector 0x{proj.id:x}")
            except XError as e:
                log(f"forward close to projector 0x{proj.id:x} failed: {e}")
        elif ev.data[1][0] == WM_TAKE_FOCUS:
            focus_child(ev.window.id)


def _stop(signum, _frame) -> None:
    log(f"signal {signum}: releasing {len(hosts)} hosted projector(s) and exiting")
    release_all()
    raise SystemExit(0)


if __name__ == "__main__":
    import signal
    if "DISPLAY" not in os.environ:
        os.environ["DISPLAY"] = ":0"
    d = display.Display()
    # A crash must never take a projector down with our host: keep our windows alive when this
    # client disconnects abnormally (a clean stop releases them explicitly in _stop/release_all);
    # the next instance re-adopts them (readopt_orphan_host).
    d.set_close_down_mode(X.RetainPermanent)
    signal.signal(signal.SIGTERM, _stop)
    signal.signal(signal.SIGINT, _stop)
    scr = d.screen()
    root = scr.root
    NET_CLIENT_LIST = d.intern_atom("_NET_CLIENT_LIST")
    NET_WM_NAME = d.intern_atom("_NET_WM_NAME")
    UTF8_STRING = d.intern_atom("UTF8_STRING")
    WM_PROTOCOLS = d.intern_atom("WM_PROTOCOLS")
    WM_DELETE_WINDOW = d.intern_atom("WM_DELETE_WINDOW")
    WM_TAKE_FOCUS = d.intern_atom("WM_TAKE_FOCUS")
    root.change_attributes(event_mask=X.PropertyChangeMask)
    log(f"started on {os.environ['DISPLAY']} (pid {os.getpid()})")
    last = 0.0
    while True:
        while d.pending_events():
            handle(d.next_event())
        now = time.monotonic()
        if now - last >= POLL_S:
            scan()
            last = now
        time.sleep(0.05)
