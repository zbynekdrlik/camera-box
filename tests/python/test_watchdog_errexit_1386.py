"""issue 1386 -- no dev1 watchdog runs a pass with errexit on.

Every dev1 alert watchdog opts out of `set -e`: a pass must survive a failing probe, parse or
assignment and keep polling on the next timer tick (the family convention, `set -uo pipefail`
without -e). A sourced lib that runs `set -euo pipefail` at source time turns -e back on in the
caller, and a later `set -uo pipefail` does NOT clear it -- only `set +e` does. avsync-heartbeat
(lib/avsync-heartbeat.sh), imag-obs (imag-obs-reachability.sh, imag-obs-restart-storm.sh) and
obs-session (win-ssh-exec.sh) ran every pass with -e on that way; the issue-1070 latency check was
killed by it once (a drift exit read as a failed assignment).

This test sources every watchdog the way its timer runs it and proves that a failing command after
the header and the source block does not end the script:
  * a watchdog whose `main` is guarded (`BASH_SOURCE[0] == $0`) is sourced with no arguments, then
    runs `false` and must still print its marker;
  * the two that end in a bare `main` (ndi-portmap, netcfg-drift) are read through `--help`, which
    exits after the source block, with an EXIT trap printing `$-`.
Network tools are stubbed (a source-time fleet lookup calls `timeout getent`). Tier-0: bash
subprocesses only.
"""
import os
import pathlib
import re
import subprocess

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_GUARD = 'if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then'
_SOURCE_RE = re.compile(r'^(?:\.|source) "\$HERE/', re.M)
_HELP_RE = re.compile(r"^\s*--help\s*(?:\||\))", re.M)


def _watchdogs():
    return sorted(p for p in _SCRIPTS.glob("*-watchdog.sh"))


def _env(tmp_path):
    stub = tmp_path / "stub"
    stub.mkdir(exist_ok=True)
    for tool in ("curl", "ssh", "sshpass", "nc", "ping", "timeout", "getent"):
        f = stub / tool
        f.write_text("#!/bin/sh\nexit 1\n")
        f.chmod(0o755)
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = dict(os.environ)
    env["HOME"] = str(home)
    env["PATH"] = f"{stub}:{env['PATH']}"
    env.pop("XDG_RUNTIME_DIR", None)
    return env


def test_every_watchdog_is_covered():
    wds = _watchdogs()
    assert len(wds) >= 28
    for wd in wds:
        text = wd.read_text()
        if _GUARD in text:
            continue
        # the bare-`main` scripts are read through --help, so --help must come after the source block
        sources = [m.start() for m in _SOURCE_RE.finditer(text)]
        helps = [m.start() for m in _HELP_RE.finditer(text)]
        assert sources and helps and helps[0] > sources[-1], (
            f"{wd.name}: no main guard, and --help is not parsed after the last source line -- "
            f"the errexit probe below could not see the source block")


_REPORT = 'echo "OPTS=$- PIPEFAIL=$(set -o | awk \'$1 == "pipefail" {print $2}\')"'


@pytest.mark.parametrize("wd", _watchdogs(), ids=lambda p: p.name)
def test_a_failing_command_after_the_source_block_does_not_end_the_watchdog(tmp_path, wd):
    env = _env(tmp_path)
    if _GUARD in wd.read_text():
        r = subprocess.run(
            ["bash", "-c", 'f="$1"; set --; . "$f"; false; echo SURVIVED; ' + _REPORT, "_", str(wd)],
            capture_output=True, text=True, env=env, timeout=60)
        assert "SURVIVED" in r.stdout, (
            f"{wd.name}: `false` after the source block ended the script (errexit is on) -- clear it "
            f"with `set +e` after the last source line.\nstderr: {r.stderr[-800:]}")
    else:
        r = subprocess.run(
            ["bash", "-c", "trap '" + _REPORT.replace("'", "'\\''") + "' EXIT; f=\"$1\"; set -- --help; . \"$f\"",
             "_", str(wd)],
            capture_output=True, text=True, env=env, timeout=60)
    m = re.search(r"^OPTS=(\S*) PIPEFAIL=(\S*)$", r.stdout, re.M)
    assert m, f"{wd.name}: no option report\n{r.stdout[-400:]}{r.stderr[-400:]}"
    opts, pipefail = m.groups()
    assert "e" not in opts, f"{wd.name} runs its pass with errexit on (options {opts!r})"
    assert "u" in opts, f"{wd.name} lost nounset (options {opts!r})"
    assert pipefail == "on", f"{wd.name} lost pipefail"
