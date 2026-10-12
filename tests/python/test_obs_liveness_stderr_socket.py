"""obs-liveness-watchdog under systemd: stderr is the journal SOCKET, not a file or a tty.

Live 7.10.-12.10.2026 (3238 passes): the probe call carried `2>>/dev/stderr`. Opening /dev/stderr for
append fails on a socket ("No such device or address"), bash then never runs the probe, and every
pass logged "no verdict lines from obs-liveness-probe.py" -- the strih/stream OBS liveness watchdog
was blind for five days. The test runs the real watchdog with its stderr on a socketpair and a fake
probe, and requires the probe's verdict to reach the decision.
"""

import os
import socket
import stat
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
WATCHDOG = REPO / "scripts" / "obs-liveness-watchdog.sh"


def _fake_probe(tmp_path):
    p = tmp_path / "fake_probe.py"
    p.write_text("import sys\nprint('HEALTHY strih-lx: activeFps=Some(30.0) avgRenderMs=Some(16.0) renderSkip=Some(0.0)')\n"
                 "print('probe note on stderr', file=sys.stderr)\n")
    p.chmod(p.stat().st_mode | stat.S_IXUSR)
    return p


def _drain(sock):
    """Read everything already written to SOCK (the writer has exited)."""
    sock.setblocking(False)
    out = b""
    while True:
        try:
            chunk = sock.recv(65536)
        except BlockingIOError:
            return out
        if not chunk:
            return out
        out += chunk


def test_the_probe_verdict_reaches_the_decision_when_stderr_is_a_socket(tmp_path):
    env = dict(os.environ, OBS_LIVENESS_PROBE=str(_fake_probe(tmp_path)), HOME=str(tmp_path))
    a, b = socket.socketpair()
    r = subprocess.run(["bash", str(WATCHDOG), "--dry-run"], stdout=subprocess.PIPE, stderr=a.fileno(),
                       env=env, text=True, timeout=60)
    a.close()
    text = r.stdout + _drain(b).decode(errors="replace")
    b.close()
    assert "No such device or address" not in text, text
    assert "no verdict lines" not in text, text
    assert "strih-lx" in text, text
