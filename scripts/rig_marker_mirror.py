#!/usr/bin/env python3
"""issue 1404 -- mirror cam2's QPSK marker log to dev1's :8890, over ONE ssh connection.

WHAT: holds one ssh connection to cam2 that announces the size of `/run/rig-qpsk-markers.csv`
(the growing marker emit log the cam2 painter writes, see .claude/rules/cam2-painter-lifecycle.md)
and then streams it with `tail -c +1 -F --pid=$PPID`. The streamed bytes become complete rows in
memory, written into the rig-lease server's serve dir (scripts/rig_serve_files.py) by temp + atomic
rename -- at most every WRITE_INTERVAL_S, only after the replay is complete, and only when the copy
has new rows. `scripts/rig-lease-server.py` serves it at `http://dev1:8890/rig-qpsk-markers.csv`
with `X-Mirror-Age-S` = seconds since the mirror last received new rows (the file's mtime): with
the painter running that is <= ~10 s; a stopped painter, a purged file (EVENT mode) or a dead
connection all read as a growing age.

WHY: restreamer's YouTube A/V gate (restreamer issue 357) pairs the QPSK markers in the VOD with
their emit times, and it has no fleet ssh -- the fleet credentials stay with camera-box (design:
issue 1404 comment 6023622339).

WHY ONE CONNECTION (issue 1404 review): one cam2 login writes 11 lines into cam2's PERSISTENT
journal on its USB stick (Storage=persistent, SystemMaxUse=200M, measured 6.10.2026). A 10 s scp
timer meant 8640 logins = ~95 000 lines a day (+61 % of the stick's journal, pushing its forensic
history out of the cap), plus a full multi-MB copy every 10 s (over the metered link when the rig
is at a venue). Here: one login per connection, then only the appended rows (~60 B/s).

STREAM SEMANTICS:
  * the remote's first stdout line is the file's size at connect time (0 when it is absent). The
    REPLAY is complete once that many bytes have arrived, or once a second session header arrives
    (the painter restarted during the replay: everything after that header is the new file, in
    order). Nothing is written before the replay is complete -- not after a stall, not when the
    connection dies or the service stops mid-replay -- so a consumer never gets a prefix. This is
    a byte count, never an idle-gap guess: a marker cadence faster than any gap would starve that.
  * `tail -c +1` replays from byte 0; `-F` follows the NAME, so a painter restart (`File::create`
    = O_TRUNC + a new header) and an EVENT-mode purge + later re-creation are followed without a
    new login; `--pid=$PPID` (the sshd session process) ends the remote tail when the connection
    drops, so no tail is ever left behind on cam2.
  * a `# qpsk-params` line (the painter's session header) starts a fresh copy; bytes before the
    first header are dropped and logged; a half row is never written; a line longer than
    PARTIAL_LINE_CAP is dropped and logged.
  * a copy equal to, or a prefix of, the served file is not written: a reconnect with no new rows
    leaves the served file and its age alone.
  * a replay not complete after REPLAY_CAP_S (the file was replaced between the size read and the
    tail's open -- a millisecond race) ends the connection and reconnects for a fresh size.
  * memory: the copy grows with the painter session (~5 MB a day); cam2 holds the same file in its
    own tmpfs, and a painter restart starts it over.
RECONNECT: a connection that ends is logged with its stderr (ERROR) and retried after a backoff
(RECONNECT_MIN_S doubling to RECONNECT_MAX_S, back to the minimum after a connection that lived
STABLE_CONNECTION_S), waited in WAIT_SLICE_S slices so a stop ends it at once (time.sleep resumes
after SIGTERM). The previous served file is kept meanwhile.

The password comes from $SSHPASS (`sshpass -e`), never the command line. Run by
`scripts/rig-marker-mirror.sh` (resolves cam2 via camera_resolve and the fleet credential).
"""
from __future__ import annotations

import argparse
import os
import selectors
import shlex
import signal
import subprocess
import sys
import time
from typing import Callable

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rig_serve_files as rsf  # noqa: E402

REMOTE_PATH = "/run/rig-qpsk-markers.csv"
HEADER_PREFIX = b"# qpsk-params"
COLUMN_HEADER = b"index,frame_id,emit_ts_ns"
WRITE_INTERVAL_S = 10.0
SELECT_TIMEOUT_S = 0.25
REPLAY_CAP_S = 120.0
PARTIAL_LINE_CAP = 4096
RECONNECT_MIN_S = 10.0
RECONNECT_MAX_S = 300.0
STABLE_CONNECTION_S = 300.0
WAIT_SLICE_S = 0.5
READ_CHUNK = 65536


def log(msg: str, stream=None) -> None:
    # airuleset:script-ok a dead stdout/stderr is the one resource this swallow is for; it cannot be logged
    try:
        print(f"{time.strftime('%Y-%m-%d %H:%M:%S')} {msg}", file=stream or sys.stdout, flush=True)
    except (OSError, ValueError):
        pass


def err(msg: str) -> None:
    log(msg, stream=sys.stderr)


class MarkerLog:
    """The in-memory copy of the marker log: complete rows only; a session header restarts it."""

    def __init__(self):
        self._buf = bytearray()
        self._partial = b""
        self._skip_to_newline = False
        self.version = 0
        self.sessions = 0
        self.dropped_bytes = 0
        self.partial_overflows = 0
        self.cut_rows = 0

    def feed(self, data: bytes) -> None:
        if not data:
            return
        lines = (self._partial + data).split(b"\n")
        self._partial = lines.pop()
        for line in lines:
            if self._skip_to_newline:  # the end of a line already dropped as too long
                self._skip_to_newline = False
                continue
            cut = line.find(HEADER_PREFIX)
            if cut > 0:
                # A truncation cut the old file mid-row and the new session's header followed on
                # the same line ('#' never occurs in a data row): the half row is dropped.
                self.cut_rows += 1
                line = line[cut:]
            if line.startswith(HEADER_PREFIX):
                self._buf = bytearray()
                self.sessions += 1
            elif self.sessions == 0:
                self.dropped_bytes += len(line) + 1
                continue
            self._buf += line + b"\n"
            self.version += 1
        if len(self._partial) > PARTIAL_LINE_CAP:
            self.partial_overflows += 1
            self._partial = b""
            self._skip_to_newline = True

    def content(self) -> bytes:
        return bytes(self._buf)

    def valid(self) -> bool:
        return COLUMN_HEADER in self._buf[:4096].split(b"\n")[:3]


def remote_command(remote_path: str) -> str:
    q = shlex.quote(remote_path)
    return f'f={q}; stat -c %s -- "$f" 2>/dev/null || echo 0; exec tail -c +1 -F --pid=$PPID -- "$f"'


def ssh_argv(host: str, remote_path: str) -> list[str]:
    return [
        "sshpass", "-e", "ssh", "-T",
        "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR",
        "-o", "ConnectTimeout=8", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3",
        f"root@{host}", remote_command(remote_path),
    ]


def spawn_ssh(host: str, remote_path: str) -> subprocess.Popen:
    return subprocess.Popen(ssh_argv(host, remote_path), stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)


def next_backoff(current: float, lived_s: float) -> float:
    if lived_s >= STABLE_CONNECTION_S:
        return RECONNECT_MIN_S
    return min(current * 2.0, RECONNECT_MAX_S)


def wait_interruptibly(seconds: float, out_of_time: Callable[[], bool], clock: Callable[[], float],
                       sleep: Callable[[float], None]) -> None:
    """Wait up to `seconds` in WAIT_SLICE_S slices, ending as soon as `out_of_time()` is true."""
    end = clock() + seconds
    while not out_of_time():
        left = end - clock()
        if left <= 0:
            return
        sleep(min(WAIT_SLICE_S, left))


def _stop_process(proc: subprocess.Popen) -> None:
    """End the ssh process group (sshpass + ssh) -- never a leftover child."""
    if proc.poll() is not None:
        return
    try:
        group = os.getpgid(proc.pid)
    except ProcessLookupError:
        return
    sig_target = (lambda s: os.killpg(group, s)) if group == proc.pid else proc.send_signal
    sig_target(signal.SIGTERM)
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        sig_target(signal.SIGKILL)
        proc.wait(timeout=3)


class _Mirror:
    """The served file and the write rule shared by every connection."""

    def __init__(self, target: str, write_interval_s: float, on_write):
        self.target = target
        self.write_interval_s = write_interval_s
        self.on_write = on_write
        found = rsf.read_mirror(target, time.time()) if os.path.lexists(target) else None
        self.served = found[0] if found else None

    def maybe_write(self, mlog: MarkerLog, now: float, last_write: float, force: bool = False) -> float:
        """Write the copy when it is valid, new (not equal to or a prefix of the served file) and
        the interval passed (or `force`). Returns the new last-write time."""
        if not mlog.valid() or (not force and now - last_write < self.write_interval_s):
            return last_write
        content = mlog.content()
        if self.served is not None and self.served.startswith(content):
            return last_write
        rsf.write_bytes_atomic(self.target, content)
        self.served = content
        if self.on_write is not None:
            self.on_write(content)
        return now


class _Connection:
    """One ssh connection: the size line, the replay count, the copy, the log lines."""

    def __init__(self, host: str, log):
        self.host = host
        self.log = log
        self.mlog = MarkerLog()
        self.announced = None
        self.head = b""
        self.received = 0
        self.replaying = True
        self.stderr_tail = b""
        self.error = None
        self._logged_dropped = 0
        self._logged_overflows = 0
        self._logged_cuts = 0

    def on_stderr(self, data: bytes) -> None:
        self.stderr_tail = (self.stderr_tail + data)[-2048:]
        for line in data.decode("utf-8", "replace").splitlines():
            if line.strip():
                self.log(f"cam2 ({self.host}): {line.strip()}")

    def on_stdout(self, data: bytes) -> None:
        if self.announced is None:
            self.head += data
            if b"\n" not in self.head:
                if len(self.head) > 64:
                    self.error = f"bad size line {self.head[:64]!r}"
                return
            line, data = self.head.split(b"\n", 1)
            if not line.strip().isdigit():
                self.error = f"bad size line {line[:64]!r}"
                return
            self.announced = int(line.strip())
        self.mlog.feed(data)
        self.received += len(data)
        if self.replaying and (self.received >= self.announced or self.mlog.sessions >= 2):
            self.replaying = False
            self.log(f"rig-marker-mirror: replay complete ({self.received} of {self.announced} announced "
                     f"bytes, {self.mlog.sessions} session header(s))")
        if self.mlog.dropped_bytes > self._logged_dropped:
            self.log(f"rig-marker-mirror: dropping {self.mlog.dropped_bytes} bytes before the first session "
                     "header (`# qpsk-params`) -- a painter without that header is never mirrored")
            self._logged_dropped = self.mlog.dropped_bytes
        if self.mlog.cut_rows > self._logged_cuts:
            self.log("rig-marker-mirror: a new session header followed a half row (the painter restarted "
                     "mid-row) -- the half row is dropped, the new session starts there")
            self._logged_cuts = self.mlog.cut_rows
        if self.mlog.partial_overflows > self._logged_overflows:
            self.log(f"rig-marker-mirror: dropped a line longer than {PARTIAL_LINE_CAP} bytes")
            self._logged_overflows = self.mlog.partial_overflows


def _follow(proc, host, mirror: _Mirror, *, clock, out_of_time, log) -> str | None:
    """Read one connection until it ends or the run stops. Returns an error reason, or None when
    the run was stopped."""
    conn = _Connection(host, log)
    conn_start = clock()
    last_write = float("-inf")
    sel = selectors.DefaultSelector()
    sel.register(proc.stdout, selectors.EVENT_READ, conn.on_stdout)
    sel.register(proc.stderr, selectors.EVENT_READ, conn.on_stderr)
    open_streams = 2
    try:
        while open_streams and conn.error is None and not out_of_time():
            for key, _mask in sel.select(timeout=SELECT_TIMEOUT_S):
                data = os.read(key.fileobj.fileno(), READ_CHUNK)
                if data:
                    key.data(data)
                else:
                    sel.unregister(key.fileobj)
                    open_streams -= 1
            now = clock()
            if conn.replaying and now - conn_start > REPLAY_CAP_S:
                conn.error = (f"replay incomplete after {REPLAY_CAP_S:.0f} s ({conn.received} of "
                              f"{conn.announced} announced bytes) -- reconnecting for a fresh size")
            elif not conn.replaying:
                last_write = mirror.maybe_write(conn.mlog, now, last_write)
        if conn.error is None and not conn.replaying:
            mirror.maybe_write(conn.mlog, clock(), last_write, force=True)  # rows since the last write
    finally:
        sel.close()
    if conn.error is None and open_streams == 0:
        try:
            proc.wait(timeout=5)  # both pipes at EOF: reap the real exit code
        except subprocess.TimeoutExpired:
            log(f"rig-marker-mirror: ssh to {host} closed its pipes but did not exit -- stopping it")
        why = conn.stderr_tail.decode("utf-8", "replace").strip().splitlines()[-1:] or ["(no stderr)"]
        return f"ssh ended rc={proc.poll()}: {why[0]}"
    return conn.error


def run(host: str, serve_dir: str, *, remote_path: str = REMOTE_PATH,
        spawn: Callable[[str, str], subprocess.Popen] = spawn_ssh,
        clock: Callable[[], float] = time.monotonic, sleep: Callable[[float], None] = time.sleep,
        wait: Callable[[float], None] | None = None,
        write_interval_s: float = WRITE_INTERVAL_S,
        max_runtime_s: float | None = None, should_stop: Callable[[], bool] = lambda: False,
        log: Callable[[str], None] = log,
        on_write: Callable[[bytes], None] | None = None) -> int:
    """The mirror loop (see the module doc). Returns 0 when stopped (signal / max_runtime_s)."""
    rsf.ensure_serve_dir(serve_dir, rsf.default_lease_dir())
    mirror = _Mirror(os.path.join(serve_dir, rsf.MARKERS_NAME), write_interval_s, on_write)
    started = clock()

    def out_of_time() -> bool:
        return should_stop() or (max_runtime_s is not None and clock() - started >= max_runtime_s)

    if wait is None:
        def wait(seconds: float) -> None:
            wait_interruptibly(seconds, out_of_time, clock, sleep)

    backoff = RECONNECT_MIN_S
    while not out_of_time():
        proc = spawn(host, remote_path)
        conn_start = clock()
        try:
            error = _follow(proc, host, mirror, clock=clock, out_of_time=out_of_time, log=log)
        finally:
            _stop_process(proc)
        if out_of_time():
            break
        lived = clock() - conn_start
        if lived >= STABLE_CONNECTION_S:
            backoff = RECONNECT_MIN_S
        log(f"ERROR rig-marker-mirror: connection to {host} ended after {lived:.1f} s: {error} "
            f"-- previous mirror kept, retry in {backoff:.0f} s")
        wait(backoff)
        backoff = next_backoff(backoff, 0.0)
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="issue 1404 -- mirror cam2's QPSK marker log to :8890")
    ap.add_argument("--host", required=True, help="cam2's address (rig-marker-mirror.sh: camera_resolve cam2)")
    ap.add_argument("--serve-dir", default=rsf.default_serve_dir(),
                    help=f"default ${rsf.SERVE_DIR_ENV} or $XDG_RUNTIME_DIR/rig-lease-serve; never the lease dir")
    ap.add_argument("--remote-path", default=REMOTE_PATH)
    ap.add_argument("--write-interval", type=float, default=WRITE_INTERVAL_S)
    ap.add_argument("--max-runtime", type=float, default=None, help="stop after N seconds (tests)")
    args = ap.parse_args(argv)
    if not os.environ.get("SSHPASS"):
        err("ERROR rig-marker-mirror: $SSHPASS (the fleet password) is not set -- refusing")
        return 2
    try:
        rsf.ensure_serve_dir(args.serve_dir, rsf.default_lease_dir())
    except (ValueError, OSError) as exc:
        err(f"ERROR rig-marker-mirror: serve dir: {exc} -- refusing")
        return 2
    stop = {"signal": None}

    def _stop(signum, _frame):
        stop["signal"] = signum  # only a flag: no I/O inside a signal handler

    signal.signal(signal.SIGTERM, _stop)
    signal.signal(signal.SIGINT, _stop)
    log(f"rig-marker-mirror: following {args.remote_path} on {args.host} -> {args.serve_dir}/{rsf.MARKERS_NAME}")
    rc = run(args.host, args.serve_dir, remote_path=args.remote_path, write_interval_s=args.write_interval,
             max_runtime_s=args.max_runtime, should_stop=lambda: stop["signal"] is not None)
    log(f"rig-marker-mirror: stopped (signal {stop['signal']})")
    return rc


if __name__ == "__main__":
    sys.exit(main())
