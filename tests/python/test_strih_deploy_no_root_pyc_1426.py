"""issue 1426 -- a Linux OBS-box deploy never leaves root-owned bytecode in its stage.

setup-strih.sh (the generated run-setup.sh child, root) and verify-strih.sh (sudo) run python from
the staged repo, so python wrote root-owned `repo/scripts/__pycache__/*.pyc` into
/tmp/genlock-stage-<sha>. The next deploy's stale-stage sweep runs as the deploy user and cannot
delete them: stream-pp logged `ERROR deleting /tmp/genlock-stage-...` for every earlier stage on
each deploy (17.10.2026, four leftovers). Both root paths now run python with
PYTHONDONTWRITEBYTECODE=1.
"""
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib" / "strih-snv-deploy.sh"


def _call(func: str, *args: str) -> str:
    out = subprocess.run(
        ["bash", "-c", 'set -euo pipefail; . "$1"; shift; "$@"', "harness", str(LIB), func, *args],
        check=True, capture_output=True, text=True,
    )
    return out.stdout


def test_the_setup_runner_child_writes_no_bytecode_1426():
    runner = _call("strih_lx_setup_runner", "/tmp/genlock-stage-abc", "stream-pp")
    child = runner[runner.index('if [ "${1:-}" = "--child" ]'):runner.index("\nfi\n")]
    assert "export PYTHONDONTWRITEBYTECODE=1" in child, child
    assert child.index("export PYTHONDONTWRITEBYTECODE=1") < child.index("setup-strih.sh"), child


def test_the_sudo_acceptance_gate_writes_no_bytecode_1426():
    cmd = _call("strih_lx_remote_accept_cmd", "/tmp/genlock-stage-abc", "stream-pp")
    assert cmd == ("sudo -k -S -p '' env PYTHONDONTWRITEBYTECODE=1 "
                   "'/tmp/genlock-stage-abc/repo/scripts/verify-strih.sh' --box stream-pp\n"), cmd
