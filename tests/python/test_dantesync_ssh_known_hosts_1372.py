"""issue 1372: the dantesync fleet tools must reach a REINSTALLED box.

Live 14.10.2026: the 1.18.0 roll refused strih-pp ("could not read the current version") because
dev1's ~/.ssh/known_hosts still held the key of the box before its reinstall. With a changed host
key ssh disables password authentication even under StrictHostKeyChecking=no, so every sshpass
read failed. Every ssh / scp the fleet upgrader and the version gate run must therefore pass
UserKnownHostsFile=/dev/null, the rig-wide convention (deploy-fleet.sh, the strih libs).
"""

import re
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[2] / "scripts"
FILES = ("dantesync-fleet-upgrade.sh", "dantesync-version-gate.sh")


def _ssh_calls(text):
    """Every `sshpass ... ssh|scp ...` call, its continuation lines joined."""
    joined = re.sub(r"\\\n\s*", " ", text)
    return [m.group(0) for m in re.finditer(r"sshpass\b[^\n]*?\b(?:ssh|scp)\b[^\n]*", joined)]


def test_every_fleet_ssh_call_ignores_a_stale_known_hosts_entry():
    for name in FILES:
        calls = _ssh_calls((SCRIPTS / name).read_text())
        assert calls, f"{name}: no sshpass ssh/scp call found -- the parser lost them"
        for call in calls:
            assert "UserKnownHostsFile=/dev/null" in call, f"{name}: {call}"
