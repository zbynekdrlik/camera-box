"""Issue 1487: camera-box installs a pinned intercom release, never another build.

scripts/lib/strih-intercom-release.sh, against a real tarball served by curl from a file:// URL (no
network, no root): the exact tag only, the pin AND the release's .sha256 both proved, the binary's
--version against the tag, nothing half-placed; step 13 installs binary, unit and TOML from that one
directory and replaces another version by rename, a running hub's inode untouched.
"""
import hashlib
import os
import subprocess
import tarfile
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "strih-intercom-release.sh"
PINNED = REPO / "tests" / "fixtures" / "intercom_pinned"
TAG = "v9.9.3"
FILES = ("intercom.strih-snv.toml", "intercom.strih-pp.toml", "intercom.stream-snv.toml",
         "intercom.stream-pp.toml", "intercom-hub.service")


def _run(script, *args, env=None):
    """The lib under the callers' strict mode; values reach it as $1.. only."""
    e = {k: v for k, v in os.environ.items() if k != "STRIH_INTERCOM_RELEASE_URL"}
    e.update(env or {})
    return subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$0"\n' + script, str(LIB), *map(str, args)],
                          capture_output=True, text=True, env=e, timeout=60)


def _hub(version):
    return "#!/bin/sh\n[ \"${1:-}\" = --version ] && echo 'intercom-hub %s' && exit 0\nexit 3\n" % version


def _release(tmp_path, tag=TAG, version=None, drop=(), said=None):
    """A release directory the file:// base serves; returns (base URL, tarball sha256)."""
    build = tmp_path / "build"
    build.mkdir(exist_ok=True)
    (build / "intercom-hub").write_text(_hub(version or tag[1:]))
    (build / "intercom-hub").chmod(0o755)
    for f in FILES:
        (build / f).write_text((PINNED / f).read_text())
    rel = tmp_path / "rel" / tag
    rel.mkdir(parents=True, exist_ok=True)
    tb = rel / "intercom-hub-linux-amd64.tar.gz"
    with tarfile.open(tb, "w:gz") as t:
        for n in ("intercom-hub",) + FILES:
            if n not in drop:
                t.add(build / n, arcname=n)
    sha = hashlib.sha256(tb.read_bytes()).hexdigest()
    (rel / (tb.name + ".sha256")).write_text("%s  %s\n" % (said or sha, tb.name))
    return "file://%s" % (tmp_path / "rel"), sha


def _tree(tmp_path, sha, tag=TAG):
    root = tmp_path / "tree"
    (root / "scripts").mkdir(parents=True)
    (root / "scripts" / "intercom-release.pin").write_text(
        "INTERCOM_RELEASE_TAG=%s\nINTERCOM_RELEASE_SHA256=%s\n" % (tag, sha))
    return root


def _ensure(root, url):
    return _run('strih_intercom_release_ensure "$1"', root, env={"STRIH_INTERCOM_RELEASE_URL": url})


def test_the_shipped_pin_is_well_formed():
    r = _run('strih_intercom_pin_tag "$1"\nstrih_intercom_pin_sha256 "$1"', REPO)
    assert r.returncode == 0, r.stderr
    tag, sha = r.stdout.split()
    assert tag.startswith("v") and len(sha) == 64


def test_the_bundle_is_fetched_verified_and_extracted(tmp_path):
    url, sha = _release(tmp_path)
    root = _tree(tmp_path, sha)
    r = _ensure(root, url)
    assert r.returncode == 0, r.stderr
    d = root / "intercom-release" / TAG
    assert r.stdout.strip() == str(d)
    for f in FILES:
        assert (d / f).read_text() == (PINNED / f).read_text()
    assert os.access(d / "intercom-hub", os.X_OK)
    assert not list(d.glob(".extract*")) and not list(d.glob("*.part"))


def test_a_present_tarball_needs_no_network(tmp_path):
    url, sha = _release(tmp_path)
    root = _tree(tmp_path, sha)
    assert _ensure(root, url).returncode == 0
    r = _ensure(root, "file:///nonexistent")
    assert r.returncode == 0, r.stderr


@pytest.mark.parametrize("case,word", [("pin", "the pin says"), ("said", "the pin says")])
def test_a_tarball_that_is_not_the_pin_is_refused_and_nothing_stays(tmp_path, case, word):
    url, sha = _release(tmp_path, said="b" * 64 if case == "said" else None)
    root = _tree(tmp_path, "c" * 64 if case == "pin" else sha)
    r = _ensure(root, url)
    assert r.returncode != 0 and word in r.stderr, r.stderr
    d = root / "intercom-release" / TAG
    assert not list(d.iterdir()), "no half-verified file stays"


def test_a_missing_release_never_falls_back(tmp_path):
    _, sha = _release(tmp_path)
    root = _tree(tmp_path, sha, tag="v9.9.4")
    r = _ensure(root, "file://%s" % (tmp_path / "rel"))
    assert r.returncode != 0 and "never another build" in r.stderr, r.stderr


def test_a_binary_of_another_version_is_refused(tmp_path):
    url, sha = _release(tmp_path, version="1.0.0")
    r = _ensure(_tree(tmp_path, sha), url)
    assert r.returncode != 0 and "not 'intercom-hub 9.9.3'" in r.stderr, r.stderr


def test_a_bundle_without_a_routing_file_is_refused(tmp_path):
    url, sha = _release(tmp_path, drop=("intercom.strih-pp.toml",))
    root = _tree(tmp_path, sha)
    r = _ensure(root, url)
    assert r.returncode != 0 and "intercom.strih-pp.toml" in r.stderr, r.stderr
    assert not (root / "intercom-release" / TAG / "intercom-hub").exists(), "nothing is placed"


def _install(tmp_path, binp):
    url, sha = _release(tmp_path)
    root = _tree(tmp_path, sha)
    d = _ensure(root, url).stdout.strip()
    return _run('strih_intercom_hub_binary_install "$1" "$2" "$3"', binp, d, root), sha


def test_a_fresh_box_gets_the_release_and_records_it(tmp_path):
    binp = tmp_path / "bin" / "intercom-hub"
    r, sha = _install(tmp_path, binp)
    assert r.returncode == 0, r.stderr
    assert binp.read_text() == _hub("9.9.3")
    rec = (tmp_path / "bin" / "intercom-hub.version").read_text()
    assert rec == "version=9.9.3\nrelease_tag=%s\ntarball_sha256=%s\n" % (TAG, sha)


def test_the_pinned_version_is_kept_as_is(tmp_path):
    binp = tmp_path / "bin" / "intercom-hub"
    binp.parent.mkdir()
    binp.write_text(_hub("9.9.3") + "# marker\n")
    binp.chmod(0o755)
    r, _ = _install(tmp_path, binp)
    assert r.returncode == 0 and "present" in r.stdout, r.stderr
    assert "# marker" in binp.read_text()
    assert not list(binp.parent.glob("*.pre-*"))


def test_another_version_is_kept_aside_and_replaced_by_rename(tmp_path):
    """A rename leaves a running hub on its old inode; it is never restarted here."""
    binp = tmp_path / "bin" / "intercom-hub"
    binp.parent.mkdir()
    binp.write_text(_hub("1.8.0"))
    binp.chmod(0o755)
    ino = binp.stat().st_ino
    r, _ = _install(tmp_path, binp)
    assert r.returncode == 0, r.stderr
    assert binp.read_text() == _hub("9.9.3") and binp.stat().st_ino != ino
    assert (binp.parent / "intercom-hub.pre-1.8.0").read_text() == _hub("1.8.0")
    assert "restart the hub" in r.stdout


def test_setup_installs_everything_from_the_one_bundle_before_the_purge_reads_it():
    s = (REPO / "scripts" / "setup-strih.sh").read_text()
    ensure = s.index('IH_BUNDLE="$(strih_intercom_release_ensure "${HERE}/..")"')
    assert ensure < s.index('step 12 "Program audio:')
    step13 = s[s.index('step 13 "'):s.index('step 14 "')]
    b = step13.index('strih_intercom_hub_binary_install /usr/local/bin/intercom-hub "$IH_BUNDLE"')
    assert b < step13.index('install -Dm644 "${IH_BUNDLE}/intercom-hub.service"')
    assert b < step13.index('strih_intercom_hub_config_install "${IH_BUNDLE}/')


def test_the_deploy_stages_the_verified_bundle_into_the_tree():
    d = (REPO / "scripts" / "lib" / "strih-snv-deploy.sh").read_text()
    a = d.index('git -C "$repo" archive --format=tar HEAD')
    e = d.index('strih_intercom_release_ensure "$w/repo"')
    c = d.index('strih_lx_tree_check "$w/repo" "$b"')
    assert a < e < c
    assert "CAMERA_BOX_SOURCE_SHA" not in d


def test_the_tree_check_needs_the_routing_file_in_the_bundle(tmp_path):
    src = (REPO / "scripts" / "lib" / "strih-snv-deploy.sh").read_text()
    assert 'strih_intercom_release_dir "$tree"' in src
    assert "the bundle must be staged" in src
