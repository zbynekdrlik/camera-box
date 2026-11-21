"""Issue 1540 slice 1 -- the per-connection video counters of one OBS box for dcontroller.

`GET :8899/video-connections.json` (bundle-state-server.py) serves `VideoFollower.snapshot()`.
The counters exist only in the OBS log, in three shapes:

  * `genlock-fifo audit '<input>': received= underruns= relocks= dropped_due= latency_ms= ...`
    every 5 s, CUMULATIVE since the source started;
  * `[distroav] jxs-source '<input>': frames= lost_slices= concealed= undecodable= camera=N ...`
    and `[distroav] jxs-output '<output>': frames= dropped_busy= ...` -- PER 5-s WINDOW;
  * `dante-input status: rx_packets= rx_loss= rx_holes= underruns= ...` -- CUMULATIVE.

The log grows ~45 KB/min, so the gather's bounded tail never covers a whole session. The follower
therefore keeps the newest log's path + byte offset + running totals, reads only the new bytes on
each call (the whole log once at start), and starts over on a new log file (a new OBS run). A
cumulative `received` that goes backwards is a source restart: that connection's `counters_since`
moves to that line. Per-window JPEG XS counters are summed since the connection's first window.

Raw counters keep their log names; the derived names of the draft contract (frames_duplicated =
underruns, frames_received = received / frames) are added only where they are exact. Pure except
for the file reads in `update`; never raises on a bad line (it is skipped).
"""
from __future__ import annotations

import datetime as _dt
import glob
import os
import re
import threading

SCHEMA = 1

_TS = re.compile(r"^(\d\d):(\d\d):(\d\d)(?:\.(\d+))?: ")
_FIELD = re.compile(r"(?:^|\s)([a-z_0-9]+)=(-?\d+(?:\.\d+)?)(?=\s|$)")
_AUDIT = re.compile(r"genlock-fifo audit '([^']+)':")
_JXS_SRC = re.compile(r"jxs-source '([^']+)':")
_JXS_OUT = re.compile(r"jxs-output '([^']+)':")
_LOG_NAME = re.compile(r"(\d{4})-(\d\d)-(\d\d) (\d\d)-(\d\d)-(\d\d)")

# The per-window JPEG XS counters that are summed since the connection's first window.
JXS_SRC_SUMMED = ("frames", "lost_slices", "concealed", "undecodable", "fec_repaired", "dropped_unread")
JXS_OUT_SUMMED = ("frames", "dropped_busy", "offered", "taken", "not_taken", "send_errors", "encode_errors")
# The cumulative genlock audit + dante counters reported as the newest value.
AUDIT_KEPT = ("received", "underruns", "relocks", "dropped_due", "late_holds", "overruns", "latency_ms")
DANTE_KEPT = ("rx_packets", "rx_loss", "rx_gaps", "rx_holes", "underruns", "overruns", "rebootstraps", "re_anchors")


def _fields(text):
    out = {}
    for k, v in _FIELD.findall(text):
        out[k] = float(v) if "." in v else int(v)
    return out


def run_start_of(log_path):
    """The OBS run's start from the log file name (`YYYY-MM-DD HH-MM-SS.txt`, local time), ISO with
    the box's UTC offset; None when the name has another shape."""
    m = _LOG_NAME.search(os.path.basename(log_path or ""))
    if not m:
        return None
    y, mo, d, h, mi, s = (int(x) for x in m.groups())
    return _dt.datetime(y, mo, d, h, mi, s).astimezone().isoformat(timespec="seconds")


class _Clock:
    """In-log `HH:MM:SS.mmm` -> ISO local time, the date from the log name, rolled over at midnight."""

    def __init__(self, log_path):
        m = _LOG_NAME.search(os.path.basename(log_path or ""))
        self.base = _dt.date(*(int(x) for x in m.groups()[:3])) if m else None
        self.days = 0
        self.last = None

    def iso(self, line):
        m = _TS.match(line)
        if not m or self.base is None:
            return None
        h, mi, s = int(m.group(1)), int(m.group(2)), int(m.group(3))
        frac = int((m.group(4) or "0")[:6].ljust(6, "0"))
        sod = h * 3600 + mi * 60 + s
        if self.last is not None and sod + 43200 < self.last:
            self.days += 1
        self.last = sod
        t = _dt.datetime.combine(self.base + _dt.timedelta(days=self.days),
                                 _dt.time(h, mi, s, frac)).astimezone()
        return t.isoformat(timespec="milliseconds")


class VideoFollower:
    """Follows the newest OBS log in a directory; `update` + `snapshot` are thread-safe."""

    def __init__(self, node):
        self.node = node
        self.lock = threading.Lock()
        self._reset(None)

    def _reset(self, path):
        self.path = path
        self.offset = 0
        self.partial = b""
        self.clock = _Clock(path)
        self.conns = {}

    def _conn(self, key, **base):
        c = self.conns.get(key)
        if c is None:
            c = dict(base)
            c["counters"] = {}
            self.conns[key] = c
        return c

    def feed_line(self, line):
        """One complete log line (no newline)."""
        t = self.clock.iso(line)
        m = _AUDIT.search(line)
        if m:
            name = m.group(1)
            f = _fields(line[m.end():])
            if "received" not in f:
                return
            c = self._conn(("in", name), sink_input=name, direction="in",
                           kind="jxs" if name.startswith("JXS ") else "ndi")
            prev = c["counters"].get("received")
            if prev is None or f["received"] < prev:
                c["counters_since"] = t
            for k in AUDIT_KEPT:
                if k in f:
                    c["counters"][k] = f[k]
            return
        m = _JXS_SRC.search(line)
        if m:
            name = m.group(1)
            f = _fields(line[m.end():])
            c = self._conn(("in", name), sink_input=name, direction="in", kind="jxs")
            c.setdefault("counters_since", t)
            c.setdefault("jxs_since", t)
            for k in JXS_SRC_SUMMED:
                c["counters"]["jxs_" + k] = c["counters"].get("jxs_" + k, 0) + int(f.get(k, 0))
            if "camera" in f:
                c["source"] = f"cam{int(f['camera'])}"
            if "decoder_opens" in f:
                c["counters"]["decoder_opens"] = int(f["decoder_opens"])
            return
        m = _JXS_OUT.search(line)
        if m:
            name = m.group(1)
            f = _fields(line[m.end():])
            if "frames" not in f:
                return
            c = self._conn(("out", name), sink_input=name, direction="out", kind="jxs")
            c.setdefault("counters_since", t)
            for k in JXS_OUT_SUMMED:
                c["counters"]["jxs_" + k] = c["counters"].get("jxs_" + k, 0) + int(f.get(k, 0))
            return
        if "dante-input status:" in line:
            f = _fields(line[line.index("dante-input status:"):])
            if "rx_packets" not in f:
                return
            c = self._conn(("in", "dante-input"), sink_input="dante-input", direction="in", kind="dante")
            prev = c["counters"].get("rx_packets")
            if prev is None or f["rx_packets"] < prev:
                c["counters_since"] = t
            for k in DANTE_KEPT:
                if k in f:
                    c["counters"][k] = f[k]

    def feed_bytes(self, data):
        buf = self.partial + data
        lines = buf.split(b"\n")
        self.partial = lines.pop()  # an unfinished last line waits for its newline
        for raw in lines:
            self.feed_line(raw.decode("utf-8", errors="replace").rstrip("\r"))

    def update(self, log_dir):
        """Read the new bytes of the newest *.txt log in LOG_DIR; a new file = a new run."""
        try:
            cands = glob.glob(os.path.join(log_dir, "*.txt"))
            if not cands:
                return
            newest = max(cands, key=os.path.getmtime)
            with self.lock:
                if newest != self.path:
                    self._reset(newest)
                size = os.path.getsize(newest)
                if size < self.offset:  # truncated in place: start over
                    self._reset(newest)
                with open(newest, "rb") as fh:
                    fh.seek(self.offset)
                    data = fh.read(size - self.offset)
                self.offset += len(data)
                self.feed_bytes(data)
        except OSError:
            return

    def snapshot(self, now=None):
        now = now or _dt.datetime.now().astimezone()
        with self.lock:
            conns = []
            for (direction, name), c in sorted(self.conns.items()):
                rec = {"id": f"{c['kind']}:{self.node}:{name}", "sink": self.node,
                       "sink_input": name, "kind": c["kind"], "direction": direction,
                       "counters_since": c.get("counters_since")}
                if "source" in c:
                    rec["source"] = c["source"]
                cnt = dict(c["counters"])
                rec.update(cnt)
                if "received" in cnt:
                    rec["frames_received"] = cnt["received"]
                if "underruns" in cnt and c["kind"] != "dante":
                    rec["frames_duplicated"] = cnt["underruns"]
                if "jxs_lost_slices" in cnt:
                    rec["slices_lost"] = cnt["jxs_lost_slices"]
                    rec["slices_concealed"] = cnt["jxs_concealed"]
                conns.append(rec)
            return {"schema": SCHEMA, "node": self.node, "kind": "obs",
                    "read_at": now.isoformat(timespec="seconds"),
                    "run_start": run_start_of(self.path), "run_id": os.path.basename(self.path or "") or None,
                    "connections": conns}
