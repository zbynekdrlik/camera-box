"""issue 1394 (21.11.2026): an in-place setup-device.sh re-run installed the camera-box binary it was
handed -- the box's OWN copy, byte-identical -- over the running process. `install -m 0755` writes a
new inode, so the running camera-box kept the deleted one mapped, STEP 18's `mount -o remount,ro /`
returned busy and PP cam1/cam4 were left with a writable root. A byte-identical binary is left in
place: `install_executable_if_changed SRC DEST` installs only when the bytes differ (or DEST is
missing), always via a new inode, and keeps an identical DEST's inode."""
import os
import subprocess
from pathlib import Path

SETUP = Path(__file__).resolve().parent.parent.parent / "scripts" / "setup-device.sh"


def _run(src, dest):
    return subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; install_executable_if_changed "$2" "$3"',
                           "h", str(SETUP), str(src), str(dest)], capture_output=True, text=True)


def test_an_identical_binary_keeps_its_inode(tmp_path):
    src, dest = tmp_path / "src", tmp_path / "dest"
    src.write_bytes(b"BINARY")
    dest.write_bytes(b"BINARY")
    dest.chmod(0o755)
    ino = dest.stat().st_ino
    r = _run(src, dest)
    assert r.returncode == 0, r.stderr
    assert dest.stat().st_ino == ino and "unchanged" in r.stdout


def test_a_different_binary_is_installed_through_a_new_inode(tmp_path):
    src, dest = tmp_path / "src", tmp_path / "dest"
    src.write_bytes(b"NEW")
    dest.write_bytes(b"OLD")
    ino = dest.stat().st_ino
    with open(dest, "rb") as held:  # like the running service: the old inode stays allocated, never reused
        r = _run(src, dest)
        assert held.read() == b"OLD", "the running copy keeps its bytes (a new inode, never a truncate)"
    assert r.returncode == 0, r.stderr
    assert dest.read_bytes() == b"NEW" and dest.stat().st_ino != ino
    assert oct(dest.stat().st_mode & 0o777) == "0o755"


def test_a_missing_destination_is_installed(tmp_path):
    src, dest = tmp_path / "src", tmp_path / "dest"
    src.write_bytes(b"NEW")
    assert _run(src, dest).returncode == 0 and dest.read_bytes() == b"NEW"


def test_every_camera_box_install_goes_through_it():
    s = SETUP.read_text()
    assert "install -m 0755 \"$BINARY_SRC\" /usr/local/bin/camera-box" not in s
    assert "install -m 0755 \"$_camera_box_dl_tmp\" /usr/local/bin/camera-box" not in s
    assert "install -m 0755 \"$DIST_DIR/camera-box\" /usr/local/bin/camera-box" not in s
    assert s.count("install_executable_if_changed") >= 4
