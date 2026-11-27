"""Issue 1425 -- the rig-NIC IRQ script is installed ATOMICALLY (temp file + rename).

strih-snv 24.11.2026: setup-strih step 11b rewrote /usr/local/bin/strih-nic-irq-affinity.sh in place
while strih-nic-irq-rebalance.timer fired 5 min after boot; the unit exec'd the half-written file
(`Exec format error`, status 203/EXEC) and verify item 16 (hk-rebalance) failed until the next boot.

Tier-0: the lib sourced by bash under the caller's strict mode, a temp dir as /usr/local/bin.
"""
import os
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "strih-nic-irq.sh"
_SETUP = _ROOT / "scripts" / "setup-strih.sh"


def _install(dest):
    return subprocess.run(
        # the renderer needs the box facts; the install's atomicity is what is under test here
        ["bash", "-c", 'set -euo pipefail\n. "$1"\n'
         'strih_nic_irq_affinity_script_text() { printf "#!/usr/bin/env bash\\necho new\\n"; }\n'
         'strih_nic_irq_affinity_script_install "$2"', "h", str(_LIB), str(dest)],
        capture_output=True, text=True, timeout=60)


def test_install_replaces_the_file_by_rename_never_in_place(tmp_path):
    dest = tmp_path / "strih-nic-irq-affinity.sh"
    dest.write_text("#!/bin/sh\necho old\n")
    old_inode = dest.stat().st_ino
    r = _install(dest)
    assert r.returncode == 0, r.stderr
    st = dest.stat()
    assert st.st_ino != old_inode, "the target was written in place, not replaced by a rename"
    assert st.st_mode & 0o777 == 0o755
    assert dest.read_text().startswith("#!/usr/bin/env bash\n")
    assert sorted(p.name for p in tmp_path.iterdir()) == [dest.name], "a temp file was left behind"


def test_install_into_a_fresh_dir_works(tmp_path):
    dest = tmp_path / "strih-nic-irq-affinity.sh"
    r = _install(dest)
    assert r.returncode == 0, r.stderr
    assert dest.stat().st_mode & 0o777 == 0o755


def test_setup_strih_step_11b_uses_the_atomic_install():
    s = _SETUP.read_text()
    assert "strih_nic_irq_affinity_script_install /usr/local/bin/strih-nic-irq-affinity.sh" in s
    assert "strih_nic_irq_affinity_script_text > /usr/local/bin/strih-nic-irq-affinity.sh" not in s
