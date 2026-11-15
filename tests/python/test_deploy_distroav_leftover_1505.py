"""Issue 1505: the Windows deploy moves a leftover Program Files DistroAV aside, fail-closed.

grading-pp (read live 15.11.2026) loads DistroAV from `C:\\Program Files\\obs-studio\\obs-plugins\\64bit\\
distroav.dll` and has no ProgramData copy. The Windows deploy program installs the pinned DistroAV only
under ProgramData (step 6b) and /XF-excludes distroav.dll from the obs-plugins robocopy, so run as it was
it either refused the box (no ProgramData load path) or, on a box with both, left two DistroAV copies for
one OBS to load. Now, on ANY Windows box:
- no ProgramData install but a Program Files copy -> the deploy creates the ProgramData install from the
  bundle (DLL + its data tree);
- a leftover Program Files distroav.dll (and its .pdb) is MOVED into the dated box-backup dir next to the
  obs.dll backup; a move that fails stops the program (exit 4) before OBS is relaunched;
- a read-back requires exactly one DistroAV copy, the ProgramData one;
- a box with no leftover logs a no-op.

These tests RUN the emitted step-6 plugin block (`genlock_plugin_deploy_ps`) in PowerShell 7 against
fixture trees: the block is dot-sourced by a harness that sets the deploy program's own variables
($obsDir, $stage, $backupDir, $manifest), with the ProgramData load path rewritten to a temp dir. A
missing pwsh FAILS (CI's ubuntu-latest ships it; dev1 has ~/.local/pwsh74/pwsh), never skips.
"""
import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "genlock-plugin-deploy.sh"
PD_LITERAL = r"C:\ProgramData\obs-studio\plugins\distroav"


def _pwsh():
    p = os.environ.get("PWSH") or shutil.which("pwsh")
    home = os.path.expanduser("~/.local/pwsh74/pwsh")
    if not p and os.access(home, os.X_OK):
        p = home
    if not p:
        pytest.fail("no pwsh: install PowerShell 7 or set PWSH=/path/to/pwsh (the emitted program must really run)")
    return p


def _block():
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; genlock_plugin_deploy_ps', "h", str(LIB)],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout


def _sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def _box(tmp, *, programdata, leftover, leftover_pdb=False, extra_leftover=None):
    """A fixture box + stage. Returns (paths dict)."""
    stage, obs, pd_root, backup = tmp / "stage", tmp / "obs", tmp / "pd" / "distroav", tmp / "backup"
    for rel, text in {"obs-plugins/64bit/distroav.dll": "NEW distroav\n",
                      "obs-plugins/64bit/obs-vban.dll": "vban\n",
                      "obs-plugins/64bit/vlc-video.dll": "vlc\n",
                      "data/obs-plugins/distroav/locale/en-US.ini": "Key=Value\n"}.items():
        (stage / rel).parent.mkdir(parents=True, exist_ok=True)
        (stage / rel).write_text(text)
    files = [{"path": rel, "sha256": _sha(stage / rel)} for rel in
             ("obs-plugins/64bit/distroav.dll", "obs-plugins/64bit/obs-vban.dll", "obs-plugins/64bit/vlc-video.dll")]
    (stage / "BUNDLE_MANIFEST.json").write_text(json.dumps({"files": files}))
    plug = obs / "obs-plugins" / "64bit"
    plug.mkdir(parents=True)
    shutil.copy(stage / "obs-plugins/64bit/obs-vban.dll", plug / "obs-vban.dll")   # what the robocopy left
    shutil.copy(stage / "obs-plugins/64bit/vlc-video.dll", plug / "vlc-video.dll")
    if leftover:
        (plug / "distroav.dll").write_text("OLD fd25 distroav\n")
    if leftover_pdb:
        (plug / "distroav.pdb").write_text("OLD pdb\n")
    if extra_leftover:
        (obs / extra_leftover).parent.mkdir(parents=True, exist_ok=True)
        (obs / extra_leftover).write_text("another stray distroav\n")
    if programdata:
        (pd_root / "bin" / "64bit").mkdir(parents=True)
        (pd_root / "bin" / "64bit" / "distroav.dll").write_text("PREVIOUS programdata distroav\n")
    backup.mkdir()
    return {"stage": stage, "obs": obs, "pd": pd_root, "backup": backup, "plug": plug}


def _run(tmp, box):
    code = [ln for ln in _block().splitlines() if not ln.lstrip().startswith("#")]
    hits = [i for i, ln in enumerate(code) if PD_LITERAL in ln]
    assert len(hits) == 1, "the code spells the ProgramData load path once (everything derives from it)"
    code[hits[0]] = code[hits[0]].replace(PD_LITERAL, str(box["pd"]))
    (tmp / "block.ps1").write_text("\n".join(code) + "\n")
    (tmp / "harness.ps1").write_text(
        "$ErrorActionPreference = 'Stop'\n"
        f"$stage = '{box['stage']}'\n$obsDir = '{box['obs']}'\n$backupDir = '{box['backup']}'\n"
        "$manifest = Join-Path $stage 'BUNDLE_MANIFEST.json'\n"
        f". '{tmp / 'block.ps1'}'\nWrite-Host 'HARNESS END'\nexit 0\n")
    return subprocess.run([_pwsh(), "-NoProfile", "-NonInteractive", "-File", str(tmp / "harness.ps1")],
                          capture_output=True, text=True, timeout=120)


def test_a_box_with_only_a_program_files_distroav_gets_the_programdata_one_and_the_leftover_moved(tmp_path):
    """The grading-pp shape: no ProgramData install, the fd25 DistroAV (+ pdb) in Program Files."""
    box = _box(tmp_path, programdata=False, leftover=True, leftover_pdb=True)
    r = _run(tmp_path, box)
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert (box["pd"] / "bin" / "64bit" / "distroav.dll").read_text() == "NEW distroav\n"
    assert (box["pd"] / "data" / "locale" / "en-US.ini").read_text() == "Key=Value\n", \
        "a ProgramData install the deploy creates gets the bundle's DistroAV data tree"
    assert not (box["plug"] / "distroav.dll").exists() and not (box["plug"] / "distroav.pdb").exists()
    assert (box["backup"] / "distroav.dll.programfiles.pre-789").read_text() == "OLD fd25 distroav\n"
    assert (box["backup"] / "distroav.pdb.programfiles.pre-789").read_text() == "OLD pdb\n"
    assert "READBACK distroav copies=1 programdata=True programfiles=0" in out
    assert "HARNESS END" in out


def test_a_box_with_both_copies_keeps_only_the_programdata_one(tmp_path):
    box = _box(tmp_path, programdata=True, leftover=True)
    r = _run(tmp_path, box)
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert (box["backup"] / "distroav.dll.pre-789").read_text() == "PREVIOUS programdata distroav\n"
    assert (box["backup"] / "distroav.dll.programfiles.pre-789").read_text() == "OLD fd25 distroav\n"
    assert not (box["plug"] / "distroav.dll").exists()
    assert "READBACK distroav copies=1 programdata=True programfiles=0" in out


def test_a_box_without_a_leftover_is_a_logged_noop(tmp_path):
    """The resolume-snv shape: ProgramData only. Nothing is moved, the deploy goes on as before."""
    box = _box(tmp_path, programdata=True, leftover=False)
    r = _run(tmp_path, box)
    out = r.stdout + r.stderr
    assert r.returncode == 0, out
    assert "no leftover DistroAV in Program Files -- nothing to move" in out
    assert sorted(p.name for p in box["backup"].iterdir()) == ["distroav.dll.pre-789"]
    assert (box["pd"] / "bin" / "64bit" / "distroav.dll").read_text() == "NEW distroav\n"
    assert not (box["pd"] / "data").exists(), "an existing ProgramData install's data tree stays unmanaged"
    assert "READBACK distroav copies=1 programdata=True programfiles=0" in out and "HARNESS END" in out


def test_a_move_that_fails_stops_the_deploy_and_leaves_the_leftover_in_place(tmp_path):
    if os.geteuid() == 0:
        pytest.fail("run as a non-root user: root ignores the read-only plugin dir this test needs")
    box = _box(tmp_path, programdata=True, leftover=True)
    # A read-only obs-plugins\64bit: the leftover cannot leave it (the stand-in for a DLL held open on
    # Windows); the backup dir stays writable, so only the move itself fails.
    os.chmod(box["plug"], 0o555)
    try:
        r = _run(tmp_path, box)
    finally:
        os.chmod(box["plug"], 0o755)
    out = r.stdout + r.stderr
    assert r.returncode == 4, out
    assert "#1505 FAIL: could not move the leftover" in out
    assert "HARNESS END" not in out, "nothing after the failed move runs"
    assert (box["plug"] / "distroav.dll").read_text() == "OLD fd25 distroav\n", "never deleted without its backup"


def test_a_second_stray_distroav_anywhere_under_obs_plugins_fails_the_readback(tmp_path):
    box = _box(tmp_path, programdata=True, leftover=False, extra_leftover="obs-plugins/32bit/distroav.dll")
    r = _run(tmp_path, box)
    out = r.stdout + r.stderr
    assert r.returncode == 4, out
    assert "READBACK distroav copies=2 programdata=True programfiles=1" in out
    assert "#1505 FAIL" in out and "HARNESS END" not in out


def test_a_box_with_no_distroav_anywhere_is_still_refused(tmp_path):
    box = _box(tmp_path, programdata=False, leftover=False)
    r = _run(tmp_path, box)
    out = r.stdout + r.stderr
    assert r.returncode != 0 and "HARNESS END" not in out, out
    assert "DistroAV is not installed where OBS loads it" in out
    assert not (box["pd"]).exists(), "nothing is created on a box that has no DistroAV at all"
