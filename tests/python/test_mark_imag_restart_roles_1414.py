"""issue 1414 follow-up: `scripts/mark-imag-restart.sh` takes every ROLE token the genlock relaunch plan
prints (`launch-obs-genlock.sh` prints `--box <role>` for strih, stream AND resolume), so the resolume
plan's last step no longer fails with "--box must be 'strih' or 'stream'"."""
import os
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "scripts" / "mark-imag-restart.sh"


def _run(tmp_path, box):
    fake = tmp_path / "bin"
    fake.mkdir()
    log = tmp_path / "ssh.log"
    ssh = fake / "ssh"
    ssh.write_text('#!/bin/bash\nprintf "%%s\\n" "$*" >> %s\n' % log)
    ssh.chmod(0o755)
    env = dict(os.environ, PATH="%s:%s" % (fake, os.environ["PATH"]), IMAG_HOST="newlevel@imag.test")
    r = subprocess.run(["bash", str(SCRIPT), "--box", box, "--reason", "test"], capture_output=True,
                       text=True, env=env, timeout=30)
    return r, (log.read_text() if log.exists() else "")


@pytest.mark.parametrize("box", ["strih", "stream", "resolume"])
def test_every_relaunch_plan_role_is_accepted(tmp_path, box):
    r, calls = _run(tmp_path, box)
    assert r.returncode == 0, r.stderr
    assert "RESTART-MARKER box=%s " % box in calls


@pytest.mark.parametrize("box", ["stream-snv", "imag", ""])
def test_anything_else_is_refused_and_nothing_is_written(tmp_path, box):
    r, calls = _run(tmp_path, box)
    assert r.returncode == 2 and calls == ""
