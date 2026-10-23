"""issue 1467: a fresh strih installs the intercom hub of its OWN checkout, and verify names a hub
older than the checkout.

23.10.2026: the FOH console's 24-bit VBAN stream was dropped silently by a hub binary built before
the 24-bit decode. `strih_intercom_hub_binary_install` took the hub from a `main` run, which was
older than the dev checkout the box was provisioned from, so a reinstalled strih would have lost FOH
again. Pinned here (Tier-0: bash + pytest, a fake curl answers the GitHub artifacts API, no network,
no root):

  * the hub artifact of the checkout's own commit is installed: the git HEAD of a checkout, or the
    `CAMERA_BOX_SOURCE_SHA` file the strih-snv deploy writes into its staged (non-git) tree;
  * `main` only when that commit has no artifact, with a WARNING naming the fallback;
  * the log names the ref used; a pre-staged dir still wins; an installed binary is still kept;
  * the installed version is recorded next to the binary (`<bin>.version`);
  * verify item 16's version row: a hub OLDER than the checkout is a NOTE with the reinstall line, a
    hub at or above it passes, nothing readable is a NOTE; a known stream with decode rejects is a
    NOTE naming the leg;
  * the deploy stages Cargo.toml + the source sha, setup-strih step 13 passes its checkout root,
    verify item 16 calls the grade.
"""
import json
import os
import subprocess
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
CI_ART = SCRIPTS / "lib" / "strih-ci-artifact.sh"
HUB_UNIT = SCRIPTS / "lib" / "strih-intercom-hub-unit.sh"
DEPLOY = SCRIPTS / "lib" / "strih-snv-deploy.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"

SHA = "1" * 40
OTHER = "2" * 40

LEAKY = ("GH_TOKEN", "STRIH_LX_HUB_SRC", "STRIH_CI_ARTIFACT_TMP", "STRIH_HUB_VERSION_JSON_FILE",
         "STRIH_HUB_DECODE_STATE_FILE", "STRIH_CI_ARTIFACT_MAX_PAGES")


def _run(script, *args, env=None, path_prefix=None):
    """Run SCRIPT under the callers' `set -euo pipefail` with ARGS as $1.. (never in the text)."""
    full = {k: v for k, v in os.environ.items() if k not in LEAKY}
    if path_prefix:
        full["PATH"] = "%s:%s" % (path_prefix, full["PATH"])
    if env:
        full.update(env)
    return subprocess.run(["bash", "-c", "set -euo pipefail\n" + script, "harness", *map(str, args)],
                          capture_output=True, text=True, env=full, cwd=str(REPO), timeout=60)


def _fake_hub(version):
    return "#!/bin/sh\n[ \"${1:-}\" = --version ] && echo 'intercom-hub %s' && exit 0\nexit 3\n" % version


def _fake_github(tmp_path, artifacts):
    """A curl for the GitHub artifacts API: page 1 = ARTIFACTS (name, head_sha, branch, version),
    page 2 = empty; each download serves a zip holding an intercom-hub that prints its version."""
    api = []
    for name, sha, branch, version in artifacts:
        with zipfile.ZipFile(tmp_path / ("%s.zip" % name), "w") as z:
            info = zipfile.ZipInfo("intercom-hub")
            info.external_attr = 0o755 << 16
            z.writestr(info, _fake_hub(version))
        api.append({"expired": False, "archive_download_url": "https://dl/%s" % name,
                    "workflow_run": {"head_branch": branch, "head_sha": sha}})
    (tmp_path / "page1.json").write_text(json.dumps({"artifacts": api}))
    (tmp_path / "page2.json").write_text(json.dumps({"artifacts": []}))
    bindir = tmp_path / "ghbin"
    bindir.mkdir()
    (bindir / "curl").write_text(
        "#!/bin/bash\nprintf 'ARGV %%s\\n' \"$*\" >> %(d)s/gh.log\n"
        "for a in \"$@\"; do [ \"$a\" = @- ] && cat > /dev/null; done\nout=\"\"; url=\"\"\n"
        "while [ $# -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift 2;; -H) shift 2;; -*) shift;; *) url=\"$1\"; shift;; esac; done\n"
        "case \"$url\" in *actions/artifacts*) p=1; case \"$url\" in *page=2*|*page=3*) p=2;; esac; cat %(d)s/page$p.json;; "
        "https://dl/*) cp %(d)s/\"${url#https://dl/}\".zip \"$out\";; *) exit 22;; esac\n" % {"d": tmp_path})
    (bindir / "curl").chmod(0o755)
    return bindir


def _staged_tree(tmp_path, sha=SHA, version="1.7.0-dev.822"):
    """A staged provisioning tree as the strih-snv deploy writes it: no .git, the source sha file."""
    root = tmp_path / "stage" / "repo"
    root.mkdir(parents=True)
    (root / "CAMERA_BOX_SOURCE_SHA").write_text(sha + "\n")
    (root / "Cargo.toml").write_text('[workspace.package]\nversion = "%s"\n' % version)
    return root


def _install(tmp_path, root, bindir, dest):
    tmproot = tmp_path / "tmproot"
    tmproot.mkdir(exist_ok=True)
    return _run('. "$1"\nstrih_intercom_hub_binary_install "$2" "$3"', CI_ART, dest, root,
                path_prefix=bindir, env={"GH_TOKEN": "tok", "STRIH_CI_ARTIFACT_TMP": str(tmproot)})


# --- the source sha + version of a checkout ---------------------------------------------------------

def test_the_source_sha_of_a_staged_tree_is_its_recorded_file(tmp_path):
    root = _staged_tree(tmp_path)
    r = _run('. "$1"\nstrih_checkout_source_sha "$2"', CI_ART, root)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == SHA


def test_the_source_sha_of_a_git_checkout_is_its_head(tmp_path):
    root = tmp_path / "co"
    root.mkdir()
    env = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t",
           "GIT_COMMITTER_EMAIL": "t@t"}
    for cmd in (["init", "-q"], ["commit", "-q", "--allow-empty", "-m", "x"]):
        subprocess.run(["git", "-C", str(root), *cmd], check=True, env={**os.environ, **env})
    head = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"], capture_output=True, text=True,
                          check=True).stdout.strip()
    (root / "CAMERA_BOX_SOURCE_SHA").write_text(OTHER + "\n")
    r = _run('. "$1"\nstrih_checkout_source_sha "$2"', CI_ART, root)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == head, "a git checkout's HEAD wins over a stray recorded file"


def test_a_tree_inside_another_repo_is_not_read_as_that_repos_head(tmp_path):
    # The staged tree under a dir that happens to be inside some other checkout must not take
    # that checkout's HEAD: only a tree that IS a work tree's top level counts.
    root = REPO / "scripts"
    r = _run('. "$1"\nstrih_checkout_source_sha "$2" || echo NONE', CI_ART, root)
    assert r.stdout.strip() == "NONE", r.stdout


def test_a_malformed_recorded_sha_is_refused(tmp_path):
    root = _staged_tree(tmp_path, sha="abc; rm -rf x")
    r = _run('. "$1"\nstrih_checkout_source_sha "$2" || echo NONE', CI_ART, root)
    assert r.stdout.strip() == "NONE"


def test_the_checkout_version_is_the_workspace_version(tmp_path):
    root = _staged_tree(tmp_path, version="1.7.0-dev.900")
    r = _run('. "$1"\nstrih_checkout_version "$2"', CI_ART, root)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "1.7.0-dev.900"
    real = _run('. "$1"\nstrih_checkout_version "$2"', CI_ART, REPO)
    assert real.stdout.strip().startswith("1.7.0-dev."), real.stdout


# --- the install ------------------------------------------------------------------------------------

def test_the_hub_of_the_checkouts_own_commit_is_installed(tmp_path):
    bindir = _fake_github(tmp_path, [("newest", OTHER, "dev", "9.9.9"), ("mine", SHA, "dev", "1.7.0-dev.822"),
                                     ("main", "3" * 40, "main", "1.7.0-dev.809")])
    dest = tmp_path / "bin" / "intercom-hub"
    r = _install(tmp_path, _staged_tree(tmp_path), bindir, dest)
    assert r.returncode == 0, r.stderr
    assert "1.7.0-dev.822" in dest.read_text(), "the artifact of the checkout's commit, not main's or the newest"
    assert ("commit %s" % SHA) in r.stdout, r.stdout
    rec = (tmp_path / "bin" / "intercom-hub.version").read_text()
    assert "version=1.7.0-dev.822\n" in rec
    assert ("source_sha=%s\n" % SHA) in rec
    assert ("artifact_ref=%s\n" % SHA) in rec


def test_main_only_when_the_commit_has_no_artifact_with_a_warning(tmp_path):
    bindir = _fake_github(tmp_path, [("newest", OTHER, "dev", "9.9.9"), ("main", "3" * 40, "main", "1.7.0-dev.809")])
    dest = tmp_path / "bin" / "intercom-hub"
    r = _install(tmp_path, _staged_tree(tmp_path), bindir, dest)
    assert r.returncode == 0, r.stderr
    assert "1.7.0-dev.809" in dest.read_text()
    assert "WARNING" in r.stderr and SHA in r.stderr and "main" in r.stderr, r.stderr
    rec = (tmp_path / "bin" / "intercom-hub.version").read_text()
    assert "artifact_ref=main\n" in rec and "version=1.7.0-dev.809\n" in rec


def test_no_checkout_commit_known_falls_back_to_main_and_says_so(tmp_path):
    bindir = _fake_github(tmp_path, [("main", "3" * 40, "main", "1.7.0-dev.809")])
    root = tmp_path / "plain"
    root.mkdir()
    dest = tmp_path / "bin" / "intercom-hub"
    r = _install(tmp_path, root, bindir, dest)
    assert r.returncode == 0, r.stderr
    assert "1.7.0-dev.809" in dest.read_text()
    assert "no source commit" in r.stderr, r.stderr


def test_an_installed_hub_is_kept_and_its_version_recorded(tmp_path):
    bindir = _fake_github(tmp_path, [("mine", SHA, "dev", "1.7.0-dev.822")])
    dest = tmp_path / "bin" / "intercom-hub"
    dest.parent.mkdir()
    dest.write_text(_fake_hub("1.7.0-dev.800"))
    dest.chmod(0o755)
    r = _install(tmp_path, _staged_tree(tmp_path), bindir, dest)
    assert r.returncode == 0, r.stderr
    assert "1.7.0-dev.800" in dest.read_text(), "an installed binary is never replaced silently"
    assert "kept" in r.stdout
    rec = (tmp_path / "bin" / "intercom-hub.version").read_text()
    assert "version=1.7.0-dev.800\n" in rec
    assert not (tmp_path / "gh.log").exists(), "nothing is fetched for a kept binary"


def test_a_prestaged_dir_still_wins_and_is_named(tmp_path):
    art = tmp_path / "art"
    art.mkdir()
    (art / "intercom-hub").write_text(_fake_hub("1.7.0-dev.700"))
    dest = tmp_path / "bin" / "intercom-hub"
    r = _run('. "$1"\nstrih_intercom_hub_binary_install "$2" "$3"', CI_ART, dest, _staged_tree(tmp_path),
             env={"STRIH_LX_HUB_SRC": str(art)})
    assert r.returncode == 0, r.stderr
    assert "1.7.0-dev.700" in dest.read_text()
    assert "pre-staged" in r.stdout, r.stdout
    assert "artifact_ref=pre-staged\n" in (tmp_path / "bin" / "intercom-hub.version").read_text()


# --- verify item 16: the version row ---------------------------------------------------------------

def _grade(tmp_path, running=None, recorded=None, checkout="1.7.0-dev.822", state=None):
    root = _staged_tree(tmp_path, version=checkout)
    binp = tmp_path / "bin" / "intercom-hub"
    binp.parent.mkdir(exist_ok=True)
    if recorded:
        (tmp_path / "bin" / "intercom-hub.version").write_text("version=%s\nsource_sha=x\nartifact_ref=main\n" % recorded)
    env = {}
    vj = tmp_path / "version.json"
    vj.write_text(json.dumps({"version": running}) if running else "")
    env["STRIH_HUB_VERSION_JSON_FILE"] = str(vj)
    sj = tmp_path / "state.json"
    sj.write_text(json.dumps(state) if state is not None else "")
    env["STRIH_HUB_DECODE_STATE_FILE"] = str(sj)
    script = ('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
              '. "$1"\n. "$2"\nstrih_intercom_hub_version_grade http://h:8790 strih-snv "$3" "$4"\necho END')
    return _run(script, CI_ART, HUB_UNIT, root, binp, env=env)


def test_a_hub_older_than_the_checkout_is_a_note_with_the_reinstall_line(tmp_path):
    r = _grade(tmp_path, running="1.7.0-dev.809")
    assert r.returncode == 0, r.stderr
    lines = [ln for ln in r.stdout.splitlines() if "1.7.0-dev.809" in ln]
    assert lines and lines[0].startswith("NOTE "), r.stdout
    assert "OLDER" in lines[0] and "1.7.0-dev.822" in lines[0]
    assert "setup-strih.sh --box strih-snv" in lines[0] and "systemctl restart intercom-hub" in lines[0]
    assert "intercom-hub.pre-1.7.0-dev.809" in lines[0], "the old binary is moved aside, never deleted"
    assert r.stdout.rstrip().endswith("END")


def test_a_hub_at_or_above_the_checkout_passes(tmp_path):
    for running in ("1.7.0-dev.822", "1.7.0-dev.830", "1.7.0", "1.8.0-dev.1"):
        r = _grade(tmp_path / running, running=running)
        assert r.returncode == 0, r.stderr
        assert any(ln.startswith("PASS ") and running in ln for ln in r.stdout.splitlines()), (running, r.stdout)


def test_versions_compare_as_numbers_not_text(tmp_path):
    r = _grade(tmp_path, running="1.7.0-dev.99", checkout="1.7.0-dev.100")
    assert any(ln.startswith("NOTE ") and "OLDER" in ln for ln in r.stdout.splitlines()), r.stdout
    r = _grade(tmp_path / "b", running="1.7.0-dev.100", checkout="1.7.0-dev.99")
    assert any(ln.startswith("PASS ") for ln in r.stdout.splitlines()), r.stdout


def test_a_hub_that_does_not_answer_is_graded_from_its_recorded_version(tmp_path):
    r = _grade(tmp_path, running=None, recorded="1.7.0-dev.800")
    line = [ln for ln in r.stdout.splitlines() if "1.7.0-dev.800" in ln]
    assert line and line[0].startswith("NOTE ") and "OLDER" in line[0] and "recorded" in line[0], r.stdout


def test_nothing_readable_is_a_note_never_a_pass(tmp_path):
    r = _grade(tmp_path, running=None, recorded=None)
    assert r.returncode == 0, r.stderr
    rows = [ln for ln in r.stdout.splitlines() if "(intercom-hub)" in ln]
    assert rows and all(ln.startswith("NOTE ") for ln in rows), r.stdout


def test_decode_rejects_on_a_known_stream_is_a_note_naming_the_leg(tmp_path):
    state = {"participants": [{"name": "fohabl", "adapter": "vban", "decode_rejects": 1234},
                              {"name": "cam1", "adapter": "vban", "decode_rejects": 0},
                              {"name": "cutters", "adapter": "pipewire"}]}
    r = _grade(tmp_path, running="1.7.0-dev.822", state=state)
    rows = [ln for ln in r.stdout.splitlines() if "decode_rejects" in ln]
    assert len(rows) == 1 and rows[0].startswith("NOTE "), r.stdout
    assert "fohabl=1234" in rows[0] and "cam1" not in rows[0]
    quiet = _grade(tmp_path / "q", running="1.7.0-dev.822", state={"participants": [{"name": "fohabl", "decode_rejects": 0}]})
    assert "decode_rejects" not in quiet.stdout, quiet.stdout


# --- the wiring --------------------------------------------------------------------------------------

def test_the_deploy_stages_cargo_toml_and_the_source_sha():
    r = subprocess.run(["bash", "-c", '. "$1" && strih_lx_tree_paths', "harness", str(DEPLOY)],
                       capture_output=True, text=True, check=True)
    assert "Cargo.toml" in r.stdout.split()
    d = DEPLOY.read_text()
    assert 'git -C "$repo" rev-parse HEAD > "$w/repo/CAMERA_BOX_SOURCE_SHA"' in d


def test_setup_step_13_installs_the_hub_of_its_checkout():
    assert 'strih_intercom_hub_binary_install /usr/local/bin/intercom-hub "${HERE}/.."' in SETUP.read_text()


def test_the_grade_defaults_to_the_checkout_it_lives_in(tmp_path):
    # verify item 16 passes no root: the grade reads the checkout its lib sits in (a staged tree
    # carries Cargo.toml since issue 1467), so the item's text needs no HERE (the item-16 harnesses
    # run it under set -u without one).
    vj = tmp_path / "v.json"
    vj.write_text(json.dumps({"version": "0.0.1"}))
    script = ('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
              '. "$1"\n. "$2"\nstrih_intercom_hub_version_grade http://h:8790 strih-snv\necho END')
    r = _run(script, CI_ART, HUB_UNIT, env={"STRIH_HUB_VERSION_JSON_FILE": str(vj),
                                             "STRIH_HUB_DECODE_STATE_FILE": str(tmp_path / "none")})
    assert r.returncode == 0, r.stderr
    want = _run('. "$1"\nstrih_checkout_version "$2"', CI_ART, REPO).stdout.strip()
    assert want and any(ln.startswith("NOTE ") and "OLDER" in ln and want in ln for ln in r.stdout.splitlines()), r.stdout


def test_verify_item_16_grades_the_hub_version():
    v = VERIFY.read_text()
    i16 = v.index("# 16) intercom hub unit")
    block = v[i16:v.index("\n# ", i16 + 10)]
    assert 'strih_intercom_hub_version_grade "http://${WS_HOST}:8790" "$(strih_box_loaded_name)"' in block
