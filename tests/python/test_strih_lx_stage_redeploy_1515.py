"""Issue 1515 (13.11.2026): a second genlock deploy at the SAME obs pin failed at [stage].

The stage dir is keyed on the obs pin sha, so a redeploy reuses it. setup-strih (root) fetched the
box's own per-box shading pin into `repo/shading-release/<tag>/`; dev1's next `rsync -a --delete`
could not unlink those root-owned files (rc 23) and the deploy stopped before anything changed.

Pinned: the repo stage rsync protects the box-fetched release caches from `--delete`, while every
file dev1 sends still arrives. A real local rsync (no ssh) runs with the deploy's own filter list
against a stage that holds a box-fetched cache. The mechanism is pinned, not the permission error:
without the filters `--delete` removes the cache (on the box it is root-owned, so that removal is
the rc 23 of 13.11). A same-user non-writable dir cannot stand in for root ownership, because rsync
makes a dir it owns writable before deleting in it.
"""

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEPLOY_LIB = ROOT / "scripts" / "lib" / "strih-snv-deploy.sh"


def _filters() -> list[str]:
    r = subprocess.run(["bash", "-c", '. "$1" && strih_lx_repo_rsync_filters', "h", str(DEPLOY_LIB)],
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


def _tree(tmp_path: Path):
    src = tmp_path / "w" / "repo"
    (src / "shading-release" / "v0.1.2").mkdir(parents=True)
    (src / "shading-release" / "v0.1.2" / "relay.tar.gz").write_text("dev1 default pin")
    (src / "scripts").mkdir()
    (src / "scripts" / "strih-obs-start.sh").write_text("new wrapper (issue 1512 shutdown watch)")
    dst = tmp_path / "stage" / "repo"
    box = dst / "shading-release" / "v0.2.5" / "bkshading-service"
    box.mkdir(parents=True)
    (box / "app.js").write_text("box-fetched REST pin")
    (dst / "scripts").mkdir()
    (dst / "scripts" / "strih-obs-start.sh").write_text("old wrapper")
    (dst / "scripts" / "gone.sh").write_text("removed on dev1")
    return src, dst


def _rsync(src: Path, dst: Path, extra: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(["rsync", "-a", "--delete", *extra, f"{src}/", f"{dst}/"],
                          capture_output=True, text=True)


def test_the_same_pin_redeploy_keeps_the_box_fetched_cache_and_delivers_dev1s_tree_1515(tmp_path):
    src, dst = _tree(tmp_path)
    r = _rsync(src, dst, _filters())
    assert r.returncode == 0, r.stderr
    assert (dst / "shading-release" / "v0.2.5" / "bkshading-service" / "app.js").exists()
    assert (dst / "shading-release" / "v0.1.2" / "relay.tar.gz").read_text() == "dev1 default pin"
    assert (dst / "scripts" / "strih-obs-start.sh").read_text() == "new wrapper (issue 1512 shutdown watch)"
    assert not (dst / "scripts" / "gone.sh").exists(), "--delete still prunes the provisioning tree"


def test_without_the_filters_delete_removes_the_box_fetched_cache_1515(tmp_path):
    # the control: plain `--delete` removes the cache (the box's root ownership turns that into rc 23),
    # so the test above proves the filters
    src, dst = _tree(tmp_path)
    r = _rsync(src, dst, [])
    assert r.returncode == 0, r.stderr
    assert not (dst / "shading-release" / "v0.2.5").exists()


def test_the_repo_stage_rsync_uses_the_filters_and_the_bundle_one_does_not_1515():
    text = DEPLOY_LIB.read_text()
    repo_call = next(l for l in text.splitlines() if '"$w/repo/" "$stage/repo/"' in l)
    bundle_call = next(l for l in text.splitlines() if '"$w/bundle/" "$stage/bundle/"' in l)
    assert "strih_lx_repo_rsync_filters" in repo_call, repo_call
    assert "strih_lx_repo_rsync_filters" not in bundle_call, bundle_call


def _child_run(tmp_path, setup_rc):
    """Run the generated run-setup.sh --child with a fake setup-strih.sh and a `chown` stub on PATH."""
    stage = tmp_path / "genlock-stage-x"
    (stage / "repo" / "scripts").mkdir(parents=True)
    (stage / "repo" / "scripts" / "setup-strih.sh").write_text(f"#!/bin/bash\nexit {setup_rc}\n")
    (stage / "repo" / "scripts" / "setup-strih.sh").chmod(0o755)
    stubs = tmp_path / "stubs"
    stubs.mkdir()
    log = tmp_path / "chown.log"
    # the stub records its argv and whether setup-strih.rc already existed (the deploy polls that file)
    (stubs / "chown").write_text(
        "#!/bin/bash\n"
        f'[ -e "{stage}/setup-strih.rc" ] && rc=present || rc=absent\n'
        f'printf "%s|rc=%s\\n" "$*" "$rc" >> "{log}"\n')
    (stubs / "chown").chmod(0o755)
    runner = subprocess.run(
        ["bash", "-c", '. "$1" && strih_lx_setup_runner "$2" strih-snv', "h", str(DEPLOY_LIB), str(stage)],
        capture_output=True, text=True)
    assert runner.returncode == 0, runner.stderr
    script = tmp_path / "run-setup.sh"
    script.write_text(runner.stdout)
    import os
    env = dict(os.environ, PATH=f"{stubs}:{os.environ['PATH']}")
    r = subprocess.run(["bash", str(script), "--child"], capture_output=True, text=True, env=env)
    return stage, log, r


def test_the_root_setup_hands_the_stage_tree_back_before_its_rc_1515(tmp_path):
    # live 13.11.2026: the sweep could not delete an older stage whose repo/ held root-owned files
    # (the box's shading pin cache, a __pycache__): each pin bump left one stage in the /tmp quota.
    stage, log, r = _child_run(tmp_path, 0)
    assert r.returncode == 0, r.stderr
    assert (stage / "setup-strih.rc").read_text().strip() == "0"
    lines = log.read_text().splitlines() if log.exists() else []
    assert lines == [f"-R --reference={stage} {stage}/repo|rc=absent"], lines


def test_a_failed_setup_still_hands_the_tree_back_and_keeps_its_rc_1515(tmp_path):
    stage, log, r = _child_run(tmp_path, 7)
    assert (stage / "setup-strih.rc").read_text().strip() == "7"
    assert log.exists() and "--reference" in log.read_text()
