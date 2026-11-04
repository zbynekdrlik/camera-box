"""Issue 1487: the Linux OBS boxes install the pinned obs release (scripts/lib/obs-release.sh).

The tarball is proved against the pin AND the release's .sha256, the manifest against the pin, every
manifest file in the unpacked tree, and the markers must name the pinned tag + commit. No fallback.
A fake release is served from file://, so nothing reaches GitHub.
"""
import hashlib
import json
import shutil
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "obs-release.sh"
TAG = "v9.9.9"
COMMIT = "c" * 40



def _sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def _lib(body, *args, env=None):
    e = {"PATH": "/usr/bin:/bin"}
    e.update(env or {})
    return subprocess.run(["bash", "-c", f"set -euo pipefail\n. '{LIB}'\n{body}", "harness", *args],
                          capture_output=True, text=True, timeout=120, env=e)


def _tree(root, commit=COMMIT, tag=TAG):
    files = {
        "bin/obs": "#!/bin/sh\necho obs\n",
        "lib/x86_64-linux-gnu/libobs.so.30": "libobs bytes\n",
        "lib/x86_64-linux-gnu/obs-plugins/dante-input.so": "dante bytes\n",
        "RUNTIME_PACKAGES.txt": "libqt6core6t64\n",
        "STRIH_BUILD_FLAGS.txt": "TARGET-RELEASE: ubuntu-26.04\n",
        "GENLOCK_BUILD_SHA.txt": commit + "\n",
        "OBS_RELEASE.txt": f"tag={tag}\ncommit={commit}\nversion={tag[1:]}\n",
    }
    for rel, text in files.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)
    (root / "bin/obs").chmod(0o755)
    return files


def _manifest(root, files, commit=COMMIT, tag=TAG):
    return {"release": {"obs_tag": tag, "obs_commit": commit, "genlock_stats_version": 4,
                        "genlock_output_stats_version": 1, "versions": {"obs-studio": "32.2.0", "distroav": "6.2.1"}},
            "schema": "camera-box/genlock-bundle-manifest@1",
            "files": [{"path": rel, "sha256": _sha(root / rel), "size": (root / rel).stat().st_size} for rel in files]}


def _release(tmp, tamper=None, in_tar_manifest=True, side_sha=None):
    """Publish a fake release under tmp/srv/<tag>/ and a pin for it; returns the env."""
    stage = tmp / "stage"
    files = _tree(stage)
    man = _manifest(stage, files)
    man_text = json.dumps(man, indent=2) + "\n"
    if in_tar_manifest:
        (stage / "BUNDLE_MANIFEST.json").write_text(man_text)
    if tamper:
        tamper(stage)
    srv = tmp / "srv" / TAG
    srv.mkdir(parents=True)
    tb = srv / f"obs-strih-linux-x86_64-{TAG}.tar.zst"
    subprocess.run(f"tar -C '{stage}' -cf - . | zstd -q -o '{tb}'", shell=True, check=True)
    (srv / "BUNDLE_MANIFEST.json").write_text(man_text)
    (srv / (tb.name + ".sha256")).write_text(f"{side_sha or _sha(tb)}  {tb.name}\n")
    pin = tmp / "obs-release.pin"
    pin.write_text(f"OBS_RELEASE_TAG={TAG}\nOBS_RELEASE_COMMIT={COMMIT}\nOBS_RELEASE_STRIH_SHA256={_sha(tb)}\n"
                   f"OBS_RELEASE_MANIFEST_SHA256={_sha(srv / 'BUNDLE_MANIFEST.json')}\n")
    return {"OBS_RELEASE_URL": f"file://{tmp / 'srv'}", "OBS_RELEASE_PIN_FILE": str(pin),
            "OBS_RELEASE_CACHE": str(tmp / "cache")}


def test_the_real_pin_is_well_formed():
    r = _lib('obs_pin_tag "$1"; obs_pin_commit "$1"; obs_pin_sha256 "$1"; obs_pin_manifest_sha256 "$1"', str(REPO))
    assert r.returncode == 0, r.stderr
    tag, commit, tsha, msha = r.stdout.split()
    assert tag.startswith("v") and len(commit) == 40 and len(tsha) == 64 and len(msha) == 64


def test_the_pinned_bundle_unpacks_verified_and_needs_no_network_the_second_time(tmp_path):
    env = _release(tmp_path)
    dest = tmp_path / "bundle"
    r = _lib('obs_release_bundle "$1" "$2"; obs_release_stats_abi "$2"; obs_release_marker_grade "$2" "$1"',
             str(REPO), str(dest), env=env)
    assert r.returncode == 0, r.stderr
    assert r.stdout.split("\n")[:2] == ["4:1", f"ok {TAG} {COMMIT}"], r.stdout
    assert (dest / "bin/obs").stat().st_mode & 0o111, "unix modes survive"
    assert json.loads((dest / "BUNDLE_MANIFEST.json").read_text())["release"]["obs_tag"] == TAG
    shutil.rmtree(tmp_path / "srv")
    dest2 = tmp_path / "bundle2"
    r = _lib('obs_release_bundle "$1" "$2"', str(REPO), str(dest2), env=env)
    assert r.returncode == 0, r.stderr


def test_a_tarball_the_pin_does_not_name_is_refused_and_nothing_stays(tmp_path):
    env = _release(tmp_path)
    pin = Path(env["OBS_RELEASE_PIN_FILE"])
    lines = [l if not l.startswith("OBS_RELEASE_STRIH_SHA256=") else "OBS_RELEASE_STRIH_SHA256=" + "0" * 64
             for l in pin.read_text().splitlines()]
    pin.write_text("\n".join(lines) + "\n")
    dest = tmp_path / "bundle"
    r = _lib('obs_release_bundle "$1" "$2"', str(REPO), str(dest), env=env)
    assert r.returncode == 1 and "the pin says" in r.stderr, r.stderr
    assert not dest.exists()
    assert not list((tmp_path / "cache").rglob("*.tar.zst")), "no unproved bytes kept"


def test_a_release_side_sha256_that_disagrees_is_refused(tmp_path):
    env = _release(tmp_path, side_sha="1" * 64)
    r = _lib('obs_release_bundle "$1" "$2"', str(REPO), str(tmp_path / "b"), env=env)
    assert r.returncode == 1 and "its .sha256 says 1111" in r.stderr, r.stderr


def test_a_file_that_differs_from_the_manifest_is_refused(tmp_path):
    def tamper(stage):
        (stage / "lib/x86_64-linux-gnu/libobs.so.30").write_text("another build\n")
    env = _release(tmp_path, tamper=tamper)
    dest = tmp_path / "b"
    r = _lib('obs_release_bundle "$1" "$2"', str(REPO), str(dest), env=env)
    assert r.returncode == 1 and "libobs.so.30 sha256" in r.stderr, r.stderr
    assert not dest.exists()


def test_a_marker_naming_another_commit_is_refused(tmp_path):
    def tamper(stage):
        (stage / "GENLOCK_BUILD_SHA.txt").write_text("d" * 40 + "\n")
    env = _release(tmp_path, tamper=tamper)
    r = _lib('obs_release_bundle "$1" "$2"', str(REPO), str(tmp_path / "b"), env=env)
    assert r.returncode == 1 and "not the pinned commit" in r.stderr and "dddd" in r.stderr, r.stderr


def test_a_tarball_manifest_other_than_the_pinned_one_is_refused(tmp_path):
    def tamper(stage):
        m = json.loads((stage / "BUNDLE_MANIFEST.json").read_text())
        m["files"] = m["files"][:1]
        (stage / "BUNDLE_MANIFEST.json").write_text(json.dumps(m))
    env = _release(tmp_path, tamper=tamper)
    r = _lib('obs_release_bundle "$1" "$2"', str(REPO), str(tmp_path / "b"), env=env)
    assert r.returncode == 1 and "not the pinned manifest" in r.stderr, r.stderr


def test_a_dante_input_plugin_is_required(tmp_path):
    def tamper(stage):
        (stage / "lib/x86_64-linux-gnu/obs-plugins/dante-input.so").unlink()
    env = _release(tmp_path, tamper=tamper)
    r = _lib('obs_release_bundle "$1" "$2"', str(REPO), str(tmp_path / "b"), env=env)
    assert r.returncode == 1 and "has no lib/x86_64-linux-gnu/obs-plugins/dante-input.so" in r.stderr, r.stderr


def test_it_never_extracts_over_a_non_empty_dir(tmp_path):
    env = _release(tmp_path)
    dest = tmp_path / "b"
    dest.mkdir()
    (dest / "keep").write_text("x")
    r = _lib('obs_release_bundle "$1" "$2"', str(REPO), str(dest), env=env)
    assert r.returncode == 1 and (dest / "keep").exists()


def test_the_marker_grade_reads_drift(tmp_path):
    d = tmp_path / "opt"
    d.mkdir()
    (d / "GENLOCK_BUILD_SHA.txt").write_text("e" * 40 + "\n")
    (d / "OBS_RELEASE.txt").write_text("tag=v0.0.1\n")
    r = _lib('obs_release_marker_grade "$1" "$2" || echo "rc=$?"', str(d), str(REPO))
    assert r.stdout.startswith("drift GENLOCK_BUILD_SHA.txt=eeee") and "rc=1" in r.stdout, r.stdout


def test_every_deploy_path_takes_the_pinned_release():
    deploy = (REPO / "scripts/lib/strih-snv-deploy.sh").read_text()
    assert 'obs_release_bundle "$repo" "$w/bundle"' in deploy
    assert "--workflow linux-genlock.yml" not in deploy.split("strih_lx_prepare() {", 1)[1].split("\n}\n", 1)[0]
    fleet = (REPO / "scripts/deploy-genlock-fleet.sh").read_text()
    assert 'strih_lx_prepare "$lx_sha"' in fleet and 'strih_lx_apply_all "$lx_sha"' in fleet
    assert 'lx_sha="$(obs_pin_commit "$HERE/..")"' in fleet
    setup = (REPO / "scripts/setup-strih.sh").read_text()
    assert 'obs_release_bundle "${HERE}/.." "$STRIH_LX_BUNDLE_SRC"' in setup
    assert "obs-release/" in (REPO / ".gitignore").read_text()


def test_the_linux_gates_grade_against_the_pin():
    assert "_e2e_linux_bytes_pinned_manifest" in (REPO / "scripts/lib/e2e-linux-bytes.sh").read_text()
    assert "manifest_autosource_fetch" not in (REPO / "scripts/lib/e2e-linux-bytes.sh").read_text().split(
        "e2e_linux_bytes_gather() {", 1)[1]
    assert "the pinned obs release %s" in (REPO / "scripts/lib/version-integrity-vendor-pin.sh").read_text()
    assert "OBS_RELEASE_OBS_STUDIO_VERSION" in (REPO / "scripts/drift-guard.sh").read_text()
