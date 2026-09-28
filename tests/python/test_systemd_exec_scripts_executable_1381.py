#!/usr/bin/env python3
"""Every repo script a checked-in systemd unit executes directly must be committed EXECUTABLE (issue 1381).

The dev1 `audio-mixer-alert-watchdog.service` runs `ExecStart=%h/devel/camera-box/scripts/
audio-mixer-alert-watchdog.sh` straight from the checkout. The script was committed as mode 100644,
so systemd failed every pass with `status=203/EXEC` from its install on 28.9.2026 onwards -- the
production-critical FOH-audio alert never ran once and nothing noticed (260 silent failures in a day).

This test walks every `systemd/*.service`, takes each Exec* line whose program is a path inside this
checkout (`%h/devel/camera-box/<rel>`), and asserts `<rel>` is tracked with git mode 100755. A unit
that runs its script through an interpreter (`/usr/bin/python3 <script>`) or a program installed
elsewhere (`/usr/local/bin/...`, provisioned by its own setup script) is out of scope.

Tier-0: stdlib only, reads `git ls-files -s`.
"""
import pathlib
import re
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
CHECKOUT_PREFIX = "%h/devel/camera-box/"
EXEC_LINE = re.compile(r"^(ExecStart|ExecStartPre|ExecStartPost|ExecStop|ExecStopPost|ExecReload)=(.*)$")


def git_modes():
    out = subprocess.run(
        ["git", "ls-files", "-s"], cwd=REPO, check=True, capture_output=True, text=True
    ).stdout
    modes = {}
    for line in out.splitlines():
        meta, path = line.split("\t", 1)
        modes[path] = meta.split()[0]
    return modes


def checkout_programs(unit_text):
    """The in-checkout program paths (relative to the repo root) a unit's Exec* lines run directly."""
    progs = []
    for raw in unit_text.splitlines():
        m = EXEC_LINE.match(raw.strip())
        if not m:
            continue
        # systemd prefixes: '-' ignore failure, '@' argv0, '+' / '!' / '!!' privileges, ':' no env expansion.
        cmd = m.group(2).lstrip("-@+!:").strip()
        if not cmd:
            continue
        prog = cmd.split()[0]
        if prog.startswith(CHECKOUT_PREFIX):
            progs.append(prog[len(CHECKOUT_PREFIX):])
    return progs


def test_checkout_programs_parses_prefixes_and_ignores_interpreters():
    unit = "\n".join([
        "[Service]",
        "ExecStartPre=-%h/devel/camera-box/scripts/a.sh --x",
        "ExecStart=%h/devel/camera-box/scripts/b.sh",
        "ExecStart=/usr/bin/python3 %h/devel/camera-box/scripts/c.py",
        "ExecStopPost=/usr/local/bin/camera-box",
        "# ExecStart=%h/devel/camera-box/scripts/commented.sh",
    ])
    assert checkout_programs(unit) == ["scripts/a.sh", "scripts/b.sh"]


def test_every_in_checkout_unit_program_is_committed_executable():
    modes = git_modes()
    units = sorted((REPO / "systemd").glob("*.service"))
    assert units, "no systemd/*.service units found -- the walk would pass vacuously"
    checked = []
    offenders = []
    for unit in units:
        for rel in checkout_programs(unit.read_text()):
            checked.append(rel)
            mode = modes.get(rel)
            if mode != "100755":
                offenders.append("%s runs %s (git mode %s)" % (unit.name, rel, mode or "UNTRACKED"))
    assert checked, "no unit runs an in-checkout program -- the prefix parse is broken"
    assert not offenders, (
        "systemd would fail these with status=203/EXEC -- commit them executable "
        "(git update-index --chmod=+x <path>):\n  " + "\n  ".join(offenders)
    )
