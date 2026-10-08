"""Issue 1404: the strih-lx genlock deploy stages every file setup-strih.sh provisions from.

Live 8.10.2026 05:53Z: `deploy-genlock-fleet.sh --boxes strih-lx` failed at setup-strih step 16e,
`vendor/av-sync-dock/src/camera-box-audio.hpp not found under /tmp/genlock-stage-<sha>/repo`. The
deploy archived only scripts/ systemd/ intercom/ vendor/realtek-r8152, while step 16e installs the
program-audio sampler from `STRIH_PROGRAM_AUDIO_FILES`, which names two vendored dock headers (the
decoder shim's sources). These tests pin that the staged tree carries every file of that list, and
that the tree check refuses a tree without them before any box is touched.
"""

import subprocess
import tarfile
import io
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEPLOY_LIB = ROOT / "scripts" / "lib" / "strih-lx-deploy.sh"
AUDIO_LIB = ROOT / "scripts" / "lib" / "strih-program-audio.sh"


def _bash(script: str) -> subprocess.CompletedProcess:
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True, cwd=ROOT)


def _tree_paths() -> list[str]:
    r = _bash(f". '{DEPLOY_LIB}' && strih_lx_tree_paths")
    assert r.returncode == 0, r.stderr
    return r.stdout.split()


def _audio_files() -> list[str]:
    r = _bash(f". '{AUDIO_LIB}' && printf '%s\\n' \"${{STRIH_PROGRAM_AUDIO_FILES[@]}}\"")
    assert r.returncode == 0, r.stderr
    files = r.stdout.split()
    assert any(f.startswith("vendor/") for f in files), files
    return files


def _covered(path: str, roots: list[str]) -> bool:
    return any(path == r or path.startswith(r.rstrip("/") + "/") for r in roots)


def test_tree_paths_cover_every_program_audio_file_1404():
    roots = _tree_paths()
    missing = [f for f in _audio_files() if not _covered(f, roots)]
    assert not missing, f"the strih-lx staged tree misses files setup-strih step 16e installs: {missing}"


def test_tree_paths_keep_the_existing_dirs_1404():
    roots = _tree_paths()
    for d in ("scripts", "systemd", "intercom", "vendor/realtek-r8152"):
        assert d in roots, roots


def test_archiving_the_tree_paths_yields_the_dock_headers_1404():
    roots = _tree_paths()
    tar_bytes = subprocess.run(
        ["git", "-C", str(ROOT), "archive", "--format=tar", "HEAD", *roots],
        capture_output=True, check=True,
    ).stdout
    names = set(tarfile.open(fileobj=io.BytesIO(tar_bytes)).getnames())
    for f in _audio_files():
        assert f in names, f"git archive of the tree paths has no {f}"


def test_tree_check_refuses_a_tree_without_the_sampler_files_1404(tmp_path):
    tree = tmp_path / "repo"
    tar_bytes = subprocess.run(
        ["git", "-C", str(ROOT), "archive", "--format=tar", "HEAD", "scripts", "systemd", "intercom"],
        capture_output=True, check=True,
    ).stdout
    tarfile.open(fileobj=io.BytesIO(tar_bytes)).extractall(tree)
    r = _bash(f". '{DEPLOY_LIB}' && strih_lx_tree_check '{tree}' strih-lx")
    assert r.returncode != 0, r.stdout
    assert "camera-box-audio.hpp" in r.stderr, r.stderr
