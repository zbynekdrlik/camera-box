#!/usr/bin/env python3
"""issue 1404 -- mirror cam2's QPSK marker log to dev1's :8890, over ONE ssh connection.

WHAT: holds one ssh connection to cam2 running `tail -c +1 -F --pid=$PPID
/run/rig-qpsk-markers.csv` (the growing marker emit log the cam2 painter writes, see
.claude/rules/cam2-painter-lifecycle.md), assembles the streamed bytes into complete rows in
memory, and writes them into the rig-lease server's serve dir (scripts/rig_serve_files.py) by temp
+ atomic rename -- at most every WRITE_INTERVAL_S and only when something changed.
`scripts/rig-lease-server.py` serves the file at `http://dev1:8890/rig-qpsk-markers.csv` with
`X-Mirror-Age-S` = seconds since the mirror last received new rows (the file's mtime): with the
painter running that is <= ~10 s; a stopped painter, a purged file (EVENT mode) or a dead
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
  * `tail -c +1` starts at byte 0, so every (re)connection replays the whole file once; `-F` follows
    the NAME, so a painter restart (`File::create` = O_TRUNC + a new header) and an EVENT-mode purge
    + later re-creation are followed without a new login; `--pid=$PPID` (the sshd session process)
    ends the remote tail when the connection drops, so no tail is ever left behind on cam2.
  * a `# qpsk-params` line (the painter's session header) starts a fresh copy; bytes before the
    first header are dropped; a half row is never written (only complete lines);
  * no write until the replayed backlog is in (the first IDLE_S without data), so a reconnect never
    serves a half-replayed copy.
RECONNECT: a connection that ends is logged with its stderr (ERROR) and retried after a backoff
(RECONNECT_MIN_S doubling to RECONNECT_MAX_S, back to the minimum after a connection that lived
STABLE_CONNECTION_S). The previous served file is kept meanwhile.

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
IDLE_S = 0.25
RECONNECT_MIN_S = 10.0
RECONNECT_MAX_S = 300.0
STABLE_CONNECTION_S = 300.0
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
        self.version = 0
        self.sessions = 0
        self.dropped_bytes = 0

    def feed(self, data: bytes) -> None:
        if not data:
            return
        lines = (self._partial + data).split(b"\n")
        self._partial = lines.pop()
        for line in lines:
            if line.startswith(HEADER_PREFIX):
                self._buf = bytearray()
                self.sessions += 1
            elif self.sessions == 0:
                self.dropped_bytes += len(line) + 1
                continue
            self._buf += line + b"\n"
            self.version += 1

    def content(self) -> bytes:
        return bytes(self._buf)

    def valid(self) -> bool:
        return COLUMN_HEADER in self._buf[:4096].split(b"\n")[:3]


def remote_command(remote_path: str) -> str:
    return f"exec tail -c +1 -F --pid=$PPID {shlex.quote(remote_path)}"


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


def run(host: str, serve_dir: str, *, remote_path: str = REMOTE_PATH,
        spawn: Callable[[str, str], subprocess.Popen] = spawn_ssh,
        clock: Callable[[], float] = time.monotonic, sleep: Callable[[float], None] = time.sleep,
        write_interval_s: float = WRITE_INTERVAL_S, idle_s: float = IDLE_S,
        max_runtime_s: float | None = None, should_stop: Callable[[], bool] = lambda: False,
        log: Callable[[str], None] = log,
        on_write: Callable[[bytes], None] | None = None) -> int:
    """The mirror loop (see the module doc). Returns 0 when stopped (signal / max_runtime_s)."""
    rsf.ensure_serve_dir(serve_dir, rsf.default_lease_dir())
    target = os.path.join(serve_dir, rsf.MARKERS_NAME)
    started = clock()

    def out_of_time() -> bool:
        return should_stop() or (max_runtime_s is not None and clock() - started >= max_runtime_s)

    backoff = RECONNECT_MIN_S
    while not out_of_time():
        proc = spawn(host, remote_path)
        conn_start = clock()
        mlog = MarkerLog()
        written_version = None
        last_write = float("-inf")
        replaying = True
        stderr_tail = b""
        sel = selectors.DefaultSelector()
        sel.register(proc.stdout, selectors.EVENT_READ, "out")
        sel.register(proc.stderr, selectors.EVENT_READ, "err")
        open_streams = 2
        while open_streams and not out_of_time():
            events = sel.select(timeout=idle_s)
            if not events:
                replaying = False
            for key, _mask in events:
                data = os.read(key.fileobj.fileno(), READ_CHUNK)
                if not data:
                    sel.unregister(key.fileobj)
                    open_streams -= 1
                elif key.data == "out":
                    mlog.feed(data)
                else:
                    stderr_tail = (stderr_tail + data)[-2048:]
                    for line in data.decode("utf-8", "replace").splitlines():
                        if line.strip():
                            log(f"cam2 ({host}): {line.strip()}")
            now = clock()
            if (not replaying and mlog.version != written_version and mlog.valid()
                    and now - last_write >= write_interval_s):
                rsf.write_bytes_atomic(target, mlog.content())
                written_version, last_write = mlog.version, now
                if on_write is not None:
                    on_write(mlog.content())
        sel.close()
        stopping = out_of_time()
        if not open_streams:  # both pipes at EOF: the ssh process is exiting -- reap its real rc
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                log(f"rig-marker-mirror: ssh to {host} closed its pipes but did not exit -- stopping it")
        _stop_process(proc)
        if mlog.version != written_version and mlog.valid():
            rsf.write_bytes_atomic(target, mlog.content())  # the rows that arrived since the last write
            if on_write is not None:
                on_write(mlog.content())
        if stopping:
            break
        lived = clock() - conn_start
        backoff = next_backoff(backoff, lived) if lived >= STABLE_CONNECTION_S else backoff
        why = stderr_tail.decode("utf-8", "replace").strip().splitlines()[-1:] or ["(no stderr)"]
        log(f"ERROR rig-marker-mirror: ssh to {host} ended rc={proc.returncode} after {lived:.1f} s: "
            f"{why[0]} -- previous mirror kept, retry in {backoff:.0f} s")
        sleep(backoff)
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
