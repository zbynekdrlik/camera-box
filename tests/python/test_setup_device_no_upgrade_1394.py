"""issue 1394 (21.11.2026): an in-place setup-device.sh re-run on a live cambox must never UPGRADE what is
already installed. On PP cam6 STEP 16's `apt-get install` upgraded systemd (via systemd-journal-remote)
and mesa (via the mpv set) on the running box; the replaced libraries stayed mapped as deleted inodes,
`mount -o remount,ro /` returned busy, and the box was left with a writable root until its next power-on.
Every STEP 16 package install therefore carries `--no-upgrade`: a missing package is still installed,
an installed one is left as it is.
"""
from pathlib import Path

SETUP = Path(__file__).resolve().parent.parent.parent / "scripts" / "setup-device.sh"


def _install_lines(*markers):
    lines = SETUP.read_text().splitlines()
    out = [ln for ln in lines if ln.lstrip().startswith("apt-get install") and any(m in ln for m in markers)]
    assert len(out) == len(markers), out
    return out


def test_the_step16_package_installs_never_upgrade_installed_packages():
    for ln in _install_lines("avahi-daemon", "ffmpeg libsdl2"):
        assert "--no-upgrade" in ln.split(), ln
