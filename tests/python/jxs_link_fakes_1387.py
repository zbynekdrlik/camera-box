"""issue 1387 slice 2 -- shared fixtures of the JPEG XS link continuity tests (not a test module).

The window lines are rendered from the SHIPPED C format strings of vendor/distroav/src/jxs-source.cpp
(`jxs_log_status`) and jxs-output.cpp (`jxs_output_log_status`), so a format change that would blind
the gate fails the tests. A line is `HH:MM:SS.mmm: [distroav] <message>`, as libobs writes the OBS log
on the Linux boxes.
"""
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
if str(REPO / "scripts") not in sys.path:
    sys.path.insert(0, str(REPO / "scripts"))
SOURCE_CPP = REPO / "vendor" / "distroav" / "src" / "jxs-source.cpp"
OUTPUT_CPP = REPO / "vendor" / "distroav" / "src" / "jxs-output.cpp"
OUTPUT = "camera-box JPEG XS program"
LOG_NAME = "2026-11-01 08-00-00.txt"


def c_format(path, head):
    """The format string starting at `head` (adjacent literals joined, PRIu64 / PRId64 -> llu)."""
    s = path.read_text()
    i = s.index(head)
    out, j = "", i
    while True:
        m = re.match(r'\s*(?:"((?:[^"\\]|\\.)*)"|(PRIu64|PRId64))', s[j:])
        if not m:
            break
        out += m.group(1) if m.group(1) is not None else "llu"
        j += m.end()
    assert out.endswith("(issue 1387)"), out
    return out


SOURCE_FMT = c_format(SOURCE_CPP, "\"jxs-source '%s': fps=%.2f")
OUTPUT_FMT = c_format(OUTPUT_CPP, "\"jxs-output '%s': fps=%.2f")

_CONV = re.compile(r"%[0-9.]*(?:llu|u|s|f|d)")
SOURCE_DEFAULTS = {"fps": "30.00", "frames": "150", "lost_slices": "0", "concealed": "0", "undecodable": "0",
                   "reopens": "0", "decoder_reopens": "0", "bpp": "8.00", "reports": "5", "report_errors": "0",
                   "dropped_unread": "0", "foreign": "0", "camera": "5", "link_mbps": "10000",
                   "fec_repaired": "0"}
OUTPUT_DEFAULTS = {"fps": "30.00", "frames": "150", "dropped_busy": "0", "offered": "150", "taken": "150",
                   "not_taken": "0", "refused": "0", "stamp_dup": "0", "encode_ms_avg/max": "9.10/11.20",
                   "first_slice_ms_avg": "0.40", "copy_ms_avg/max": "0.31/0.60", "bpp": "8", "auto": "on",
                   "reports": "5", "report_age_s": "0.4", "send_errors": "0", "encode_errors": "0",
                   "pace_mbps": "900", "wire_ms_avg/max": "19.80/21.10", "pace_src": "receiver-link",
                   "rx_link_mbps": "1000", "tx_link_mbps": "10000", "path": "clean", "cong_pct": "100",
                   "queue_ms": "0.1", "fec_sent": "1380", "fec_repaired": "0"}


def _render(fmt, name, defaults, values):
    text = fmt.replace("'%s'", f"'{name}'", 1)
    out, pos = [], 0
    for m in re.finditer(r"([a-z_/]+)=((?:%[0-9.]*(?:llu|u|s|f|d)[./]?)+)", text):
        key = m.group(1)
        assert key in defaults, f"the shipped format has a key the fixture does not know: {key}"
        out.append(text[pos:m.start()] + f"{key}={values.get(key, defaults[key])}")
        pos = m.end()
    out.append(text[pos:])
    line = "".join(out)
    assert not _CONV.search(line), line
    return line


def source_line(t, name, **values):
    return f"{t}: [distroav] " + _render(SOURCE_FMT, name, SOURCE_DEFAULTS, {k: str(v) for k, v in values.items()})


def output_line(t, name=OUTPUT, **values):
    return f"{t}: [distroav] " + _render(OUTPUT_FMT, name, OUTPUT_DEFAULTS, {k: str(v) for k, v in values.items()})


def ts(sec):
    sec = sec % 86400
    whole = int(sec)
    return f"{whole // 3600:02d}:{whole // 60 % 60:02d}:{whole % 60:02d}.{int(round((sec - whole) * 1000)) % 1000:03d}"


def noise(t, k=0):
    """An ordinary OBS log line (another subsystem) between the window lines."""
    return f"{t}: [obs-vban] obs-vban receive: received={1000 + k} lost=0 late=0 stream='stream' source='mbc'"


def window_set(t, names, output=True, src=None, out=None, filler=3):
    """One 5 s window's lines at time t: a few other lines, then each input's line (+ the output's)."""
    src = src or {}
    out = out or {}
    lines = [noise(ts(t - 0.5 + 0.1 * k), k) for k in range(filler)]
    for n in names:
        lines.append(source_line(ts(t), n, **src.get(n, {})))
    if output:
        lines.append(output_line(ts(t + 0.004), **out))
    return lines


class Log:
    """An OBS log being written: lines appended, byte marks taken like the box's `stat -c %s`."""

    def __init__(self):
        self.data = b""

    def add(self, lines):
        for line in lines:
            self.data += line.encode() + b"\n"

    def mark(self):
        return len(self.data)


CAMS = ["JXS cam2 src", "JXS cam3 src", "JXS cam5 src"]
PGM = "JXS PGM src"
_GREP = re.compile(rb"^[0-9:.]+: \[distroav\] jxs-(source|output) ")
T0 = 10 * 3600


def box_slice(log, from_off, length, name=LOG_NAME, newest=None):
    """What the box prints for the slice: the header, then `grep -a -b` of [from, from + length)."""
    out = f"#newest\t{newest or name}\t{len(log.data)}\n".encode()
    chunk = log.data[from_off:from_off + length]
    off = 0
    for line in chunk.split(b"\n"):
        if _GREP.match(line):
            out += str(off).encode() + b":" + line + b"\n"
        off += len(line) + 1
    return out


def run_box(log, start, stop, names=CAMS, program_output=True, back=4096, close=4096, newest=None,
            start_time=None, stop_time=None, role="strih"):
    """The gate's read of one box (from = start - back, len = stop - from + close) -> its result."""
    from_off = max(0, start - back)
    length = stop - from_off + close
    data = box_slice(log, from_off, length, newest=newest)
    import jxs_link_continuity as jlc  # the module under test, imported where it is used
    return jlc.decide_box(role, data, LOG_NAME, from_off, length, start, stop, start_time or "",
                          stop_time or "", names, program_output)


def recording(windows=8, src=None, out=None, names=CAMS, output=True, pre=3, post=3, gap_at=None,
              t0=T0, extra=None, closing_out=None, closing_src=None):
    """A log with `pre` windows before the start, `windows` inside, `post` after the stop. `src` /
    `out` override one window's counters: {index: {name: {...}}} / {index: {...}} (index 0 = the
    first window inside). Returns (log, start, stop, start_time, stop_time)."""
    log = Log()
    t = t0
    for _ in range(pre):
        log.add(window_set(t, names, output))
        t += 5.0
    start_time = ts(t - 4.9)
    log.add([noise(start_time)])
    start = log.mark()
    for k in range(windows):
        if gap_at is not None and k == gap_at:
            t += 5.0  # one window line missing
        log.add(window_set(t, names, output, src=(src or {}).get(k), out=(out or {}).get(k)))
        if extra and k in extra:
            log.add(extra[k])
        t += 5.0
    stop_time = ts(t - 4.0)
    log.add([noise(stop_time)])
    stop = log.mark()
    for k in range(post):
        log.add(window_set(t, names, output, src=closing_src if k == 0 else None, out=closing_out if k == 0 else None))
        t += 5.0
    return log, start, stop, start_time, stop_time
