#!/usr/bin/env python3
"""obs-box-headphones: the stream operator's GLOBAL headphone volume (issue 1465 item 3 revision).

WHY (owner 24.10.2026, ROZHODNUTE 6383739315, verbatim): "... tiez musi byt zrozumitelne ako si ma operator
regulovat hlasitost sluchatiek, nie len tu localnu v vlc/chrome yt ale aj tu globalnu". Design 6383742509
(Approach 1): ONE control for the ONE headphone output, nothing else the operator could switch away -- a generic
mixer tray (pavucontrol, pasystray) exposes every card and every per-app stream, which the owner banned ("nesmie
sa babrat do inych veci").

WHAT: acts on the sink `stream-operator-headphones` ONLY (the locked operator PipeWire's one sink, rendered by
scripts/stream_operator_audio.py), found BY NAME in pw-dump on every call (a node id changes with every PipeWire
restart, and wpctl takes only an id), never the default-sink alias, never another node. Volumes are wpctl's cubic
scale (what `wpctl get-volume` prints), every change capped at 1.0 (`wpctl set-volume -l 1.0`: no boost).
  watch [--interval S] [--polls N]  the tint2 panel item (a continuous executor): one line per change,
                                    `Slúchadlá NN %` / `Slúchadlá stlmené` / `Slúchadlá nedostupné`. When a
                                    sink this box never restored appears (core cookie + node id: a login after
                                    a reboot, an operator-audio restart) it puts the KEPT level back (at least
                                    10 %) and unmutes (every session starts audible); a restarted watcher adopts
                                    a sink it already restored as it is; any later change (a key press, another
                                    client) is kept as the new level. One look a second: one pw-dump (~11 ms on
                                    stream-pp, 24.10.2026), a client that creates no node.
  up | down                         +-5 % (`up` also unmutes), the level kept; bound to the wheel and to
                                    XF86AudioRaiseVolume / XF86AudioLowerVolume in the kiosk's openbox rc.xml
  toggle                            mute / unmute; the left click and XF86AudioMute
  print                             the label, nothing written
The kept state is one JSON file, $XDG_STATE_HOME/camera-box/operator-headphones.json (default
~/.local/state/...): `volume` (the kept level) + `restored` (the key of the sink last restored); only this sink's
volume is restored; WirePlumber's own restore stays off for everything.
Exit 1 when the sink is not there (label `Slúchadlá nedostupné`, nothing touched) or a wpctl call failed.

Installed by setup-strih step 12 on the STREAM role as /usr/local/bin/obs-box-headphones
(scripts/lib/stream-operator-audio.sh); the panel item + keys are declared in scripts/lib/obs-box-panel.sh.
Seams: STREAM_OPERATOR_VOLUME_PW_DUMP / _WPCTL (the tools), STREAM_OPERATOR_VOLUME_STATE (the kept-level file).
Std-only.
"""
import argparse
import json
import os
import subprocess
import sys
import time

SINK_NAME = "stream-operator-headphones"
STEP_PCT = 5
LIMIT = "1.0"
LABEL_GONE = "Slúchadlá nedostupné"
LABEL_MUTED = "Slúchadlá stlmené"
# a kept level is never restored below this: every session starts audible
RESTORE_MIN = 0.10
# the sink counts as gone only after this many missed looks in a row
GONE_AFTER = 3
PW_DUMP = os.environ.get("STREAM_OPERATOR_VOLUME_PW_DUMP", "pw-dump")
WPCTL = os.environ.get("STREAM_OPERATOR_VOLUME_WPCTL", "wpctl")
TOOL_TIMEOUT_S = 10


def state_path():
    """The kept-level file."""
    p = os.environ.get("STREAM_OPERATOR_VOLUME_STATE")
    if p:
        return p
    base = os.environ.get("XDG_STATE_HOME") or os.path.join(os.path.expanduser("~"), ".local", "state")
    return os.path.join(base, "camera-box", "operator-headphones.json")


def read_sink(objs):
    """{id, key, volume (cubic), mute} of the Audio/Sink node named SINK_NAME in a pw-dump list, else None.
    KEY = `<core cookie>:<node id>`: PipeWire hands node ids out again after a restart, and its core cookie is
    random per daemon start, so the pair names ONE sink of ONE PipeWire run (review round 1)."""
    objs = objs if isinstance(objs, list) else []
    cookie = None
    for o in objs:
        if isinstance(o, dict) and o.get("type") == "PipeWire:Interface:Core":
            cookie = (o.get("info") or {}).get("cookie")
            break
    for o in objs:
        if not isinstance(o, dict) or o.get("type") != "PipeWire:Interface:Node":
            continue
        info = o.get("info") or {}
        props = info.get("props") or {}
        if props.get("node.name") != SINK_NAME or props.get("media.class") != "Audio/Sink":
            continue
        for p in ((info.get("params") or {}).get("Props") or []):
            chans = [v for v in (p.get("channelVolumes") or []) if _is_number(v)] if isinstance(p, dict) else []
            if chans:
                linear = max(sum(chans) / len(chans), 0.0)
                return {"id": o.get("id"), "key": "%s:%s" % (cookie if cookie is not None else "?", o.get("id")),
                        "volume": linear ** (1.0 / 3), "mute": p.get("mute") is True}
        return None
    return None


def _is_number(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def label(sink):
    """The panel text in plain Slovak."""
    if sink is None:
        return LABEL_GONE
    if sink["mute"]:
        return LABEL_MUTED
    return "Slúchadlá %d %%" % round(sink["volume"] * 100)


def pw_sink():
    """The operator sink now, from pw-dump; None when it (or PipeWire) is not there."""
    try:
        r = subprocess.run([PW_DUMP], capture_output=True, text=True, timeout=TOOL_TIMEOUT_S)
        if r.returncode != 0:
            return None
        return read_sink(json.loads(r.stdout or "null"))
    except (OSError, subprocess.TimeoutExpired, ValueError):
        return None


def wpctl(*args):
    """One wpctl call; a failure is written to stderr (the panel and the keys never block on it)."""
    try:
        r = subprocess.run([WPCTL, *args], capture_output=True, text=True, timeout=TOOL_TIMEOUT_S)
    except (OSError, subprocess.TimeoutExpired) as e:
        print("obs-box-headphones: wpctl %s failed: %s" % (" ".join(args), e), file=sys.stderr)
        return False
    if r.returncode != 0:
        print("obs-box-headphones: wpctl %s failed (rc %d): %s"
              % (" ".join(args), r.returncode, (r.stderr or r.stdout or "").strip()), file=sys.stderr)
        return False
    return True


def load_state(path):
    """The kept state {volume, restored} (an unreadable file reads empty)."""
    try:
        with open(path) as fh:
            d = json.load(fh)
    except (OSError, ValueError):
        return {}
    return d if isinstance(d, dict) else {}


def load_level(path):
    """The kept level (0..1) or None."""
    v = load_state(path).get("volume")
    return min(max(float(v), 0.0), 1.0) if _is_number(v) else None


def save_state(path, **updates):
    """Merge UPDATES into the kept state atomically (a volume rounded to 1 %); a failure is logged, never fatal."""
    d = load_state(path)
    if "volume" in updates:
        updates["volume"] = round(min(max(updates["volume"], 0.0), 1.0), 2)
    d.update(updates)
    try:
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        tmp = "%s.tmp-%d" % (path, os.getpid())
        with open(tmp, "w") as fh:
            json.dump(d, fh)
        os.replace(tmp, path)
    except OSError as e:
        print("obs-box-headphones: cannot keep the level in %s: %s" % (path, e), file=sys.stderr)


def restore(call, sink, path):
    """Put the kept level back on SINK (when one is kept; never below RESTORE_MIN, so the session is audible),
    unmute it, and remember that THIS sink (its key) got its restore: a restarted watcher (a panel restart)
    then adopts the sink as it is, the operator's own mute included."""
    kept = load_level(path)
    if kept is not None:
        call("set-volume", "-l", LIMIT, str(sink["id"]), "%.2f" % max(kept, RESTORE_MIN))
        save_state(path, restored=sink["key"])
    else:
        # nothing kept yet (the first session on this box): the level the sink has now is the one kept
        save_state(path, restored=sink["key"], volume=sink["volume"])
    call("set-mute", str(sink["id"]), "0")


class Watcher:
    """The panel item's loop body: READ() -> the sink or None, CALL(*wpctl args), PATH = the kept-level file.

    A sink whose key (core cookie + node id) this box never restored is NEW (a login after a reboot, an
    operator-audio restart): its kept level goes back and it is unmuted. A sink seen again (the same key, also
    after a panel restart) is adopted as it is, and a level change on it is kept. The sink counts as gone only
    after GONE_AFTER missed looks in a row, so one slow pw-dump never flashes the label or re-runs the restore."""

    def __init__(self, read, call, path):
        self.read = read
        self.call = call
        self.path = path
        self.key = None
        self.last_volume = None
        self.misses = 0
        self.shown = None

    def poll(self):
        """One look; returns the label when it changed since the last one, else None."""
        s = self.read()
        if s is None:
            self.misses += 1
            if self.misses < GONE_AFTER:
                return None
            self.key = None
            return self._show(None)
        self.misses = 0
        if s["key"] != self.key:
            self.key = s["key"]
            if load_state(self.path).get("restored") != s["key"]:
                restore(self.call, s, self.path)
                s = self.read() or s
            self.last_volume = s["volume"]
        elif abs(s["volume"] - self.last_volume) >= 0.005:
            save_state(self.path, volume=s["volume"])
            self.last_volume = s["volume"]
        return self._show(s)

    def _show(self, s):
        text = label(s)
        if text == self.shown:
            return None
        self.shown = text
        return text


def _change(verb):
    s = pw_sink()
    if s is None:
        print(LABEL_GONE)
        return 1
    sid = str(s["id"])
    ok = True
    if verb in ("up", "down"):
        ok = wpctl("set-volume", "-l", LIMIT, sid, "%d%%%s" % (STEP_PCT, "+" if verb == "up" else "-"))
        if verb == "up" and s["mute"]:
            ok = wpctl("set-mute", sid, "0") and ok
    elif verb == "toggle":
        ok = wpctl("set-mute", sid, "toggle")
    after = pw_sink() or s
    if verb != "toggle":
        save_state(state_path(), volume=after["volume"])
    print(label(after))
    return 0 if ok else 1


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    w = sub.add_parser("watch")
    w.add_argument("--interval", type=float, default=1.0)
    w.add_argument("--polls", type=int, default=0, help="stop after N looks (0 = run until killed)")
    for verb in ("up", "down", "toggle", "print"):
        sub.add_parser(verb)
    a = ap.parse_args(argv)
    if a.cmd == "print":
        s = pw_sink()
        print(label(s))
        return 0 if s is not None else 1
    if a.cmd != "watch":
        return _change(a.cmd)
    watcher = Watcher(pw_sink, wpctl, state_path())
    n = 0
    while True:
        line = watcher.poll()
        if line is not None:
            print(line, flush=True)
        n += 1
        if a.polls and n >= a.polls:
            return 0
        time.sleep(max(a.interval, 0.0))


if __name__ == "__main__":
    sys.exit(main())
