"""The E2E [0/8] exhaustive normalize burn sweep never dials an acked-offline imag-nb (issue 1013).

18.10.2026, release E2E run 39066977175 attempt 2: the `#938/#1011` normalize sweep in
recording-e2e.sh (`for _nsip in "$STRIH" "$STREAM" "$IMAG_IP"`) dialed imag-nb, which is acked
offline in rig-fleet.txt (owner 20.9.2026), and printed a full python traceback
(`OSError: [Errno 113] No route to host`) into every run's log. Every other imag site skips with the
loud `imag_leg_skip_note`; this one must too, and must still sweep strih + stream.

The loop is cut out of the real script and RUN with a fake python3 that logs the host it was given.
"""
import os
import re
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
E2E = REPO / "scripts" / "recording-e2e.sh"
ACK_LIB = REPO / "scripts" / "lib" / "imag-offline-ack.sh"


def _loop_text():
    s = E2E.read_text()
    start = s.index('  for _nsip in "$STRIH" "$STREAM" "$IMAG_IP"; do')
    end = s.index("\n  done\n", start) + len("\n  done\n")
    return s[start:end]


def _run(tmp_path, acked):
    fake = tmp_path / "bin"
    fake.mkdir()
    log = tmp_path / "hosts.log"
    py = fake / "python3"
    py.write_text(
        "#!/bin/bash\n"
        'while [ $# -gt 0 ]; do if [ "$1" = --host ]; then echo "$2" >> "$HOSTLOG"; fi; shift; done\n'
        "exit 0\n"
    )
    py.chmod(0o755)
    script = (
        "set -euo pipefail\n"
        f'. "{ACK_LIB}"\n'
        'HERE=/nonexistent; OBS_CLEANUP_TIMEOUT=5\n'
        'STRIH=10.77.9.202; STREAM=10.77.9.204; IMAG_IP=10.77.9.182\n'
        f'IMAG_OFFLINE_ACKED={1 if acked else 0}; IMAG_OFFLINE_ACK_REASON="owner test reason"\n'
        + _loop_text()
        + 'echo "LOOP-END"\n'
    )
    env = dict(os.environ, PATH=f"{fake}:{os.environ['PATH']}", HOSTLOG=str(log))
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=env, timeout=60)
    hosts = log.read_text().split() if log.exists() else []
    return r, hosts


def test_an_acked_offline_imag_is_never_dialed_and_the_skip_is_named(tmp_path):
    r, hosts = _run(tmp_path, acked=True)
    assert r.returncode == 0, r.stderr
    assert "LOOP-END" in r.stdout
    assert hosts == ["10.77.9.202", "10.77.9.204"], hosts
    assert re.search(r"IMAG-LEG-SKIPPED: .*normalize.*owner test reason", r.stdout), r.stdout


def test_a_present_imag_is_still_swept(tmp_path):
    r, hosts = _run(tmp_path, acked=False)
    assert r.returncode == 0, r.stderr
    assert hosts == ["10.77.9.202", "10.77.9.204", "10.77.9.182"], hosts
    assert "IMAG-LEG-SKIPPED" not in r.stdout
