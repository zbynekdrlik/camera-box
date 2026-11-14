"""Issue 1487: a stand-in obs release for the deploy tests (not a test module).

The Linux OBS boxes take the pinned obs release (scripts/lib/obs-release.sh), so a test that runs the
deploy hands it a pin + a release already in its cache dir (the lib's OBS_RELEASE_PIN_FILE /
OBS_RELEASE_CACHE seams; OBS_RELEASE_URL points nowhere, so nothing reaches GitHub). The release's
commit is the test's own short SHA, which the deploy then uses as the Linux boxes' canonical SHA.
"""
import atexit
import hashlib
import json
import shutil
import subprocess
import tempfile
from pathlib import Path

TAG = "v9.9.5"


def _sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def _pin_text(commit, tarball_sha="0" * 64, manifest_sha="0" * 64, tag=TAG):
    return ("OBS_RELEASE_TAG=%s\nOBS_RELEASE_COMMIT=%s\nOBS_RELEASE_STRIH_SHA256=%s\n"
            "OBS_RELEASE_MANIFEST_SHA256=%s\nOBS_RELEASE_OBS_STUDIO_VERSION=32.2.0\n"
            "OBS_RELEASE_DISTROAV_VERSION=6.2.1\n" % (tag, commit, tarball_sha, manifest_sha))


def fake_obs_pin_env(commit):
    """A pin only (the plan reads the tag + commit, never the bytes): {OBS_RELEASE_PIN_FILE: ...}."""
    d = Path(tempfile.mkdtemp(prefix="fake-obs-pin-"))
    atexit.register(shutil.rmtree, d, True)
    pin = d / "obs-release.pin"
    # issue 1505: a Windows box deploys only from the pin's Windows zip, so the plan pin names one too
    pin.write_text(_pin_text(commit) + "OBS_RELEASE_WINDOWS_SHA256=%s\nOBS_RELEASE_WINDOWS_MANIFEST_SHA256=%s\n"
                   % ("0" * 64, "0" * 64))
    return {"OBS_RELEASE_PIN_FILE": str(pin)}


def fake_obs_release_env(tmp_path, commit, artifact_sha=None):
    """A whole release in its cache + the matching pin under TMP_PATH. Returns (env, the libobs.so.30
    sha256). ARTIFACT_SHA writes another commit into the bundle's marker (a foreign build)."""
    tmp = Path(tmp_path)
    stage = tmp / "fake-obs-stage"
    files = {
        "bin/obs": "#!/bin/sh\necho obs\n",
        "lib/x86_64-linux-gnu/libobs.so.30": "fake libobs\n",
        "lib/x86_64-linux-gnu/obs-plugins/dante-input.so": "fake dante\n",
        "RUNTIME_PACKAGES.txt": "libqt6core6t64\n",
        "STRIH_BUILD_FLAGS.txt": "TARGET-RELEASE: ubuntu-26.04\n",
        "GENLOCK_BUILD_SHA.txt": (artifact_sha or commit) + "\n",
        "OBS_RELEASE.txt": "tag=%s\ncommit=%s\nversion=%s\n" % (TAG, commit, TAG[1:]),
    }
    for rel, text in files.items():
        p = stage / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)
    man = {"release": {"obs_tag": TAG, "obs_commit": commit, "genlock_stats_version": 4,
                       "genlock_output_stats_version": 1},
           "files": [{"path": rel, "sha256": _sha(stage / rel)} for rel in files]}
    man_text = json.dumps(man, indent=2) + "\n"
    (stage / "BUNDLE_MANIFEST.json").write_text(man_text)
    rel_dir = tmp / "fake-obs-cache" / TAG
    rel_dir.mkdir(parents=True)
    tb = rel_dir / ("obs-strih-linux-x86_64-%s.tar.zst" % TAG)
    subprocess.run(["bash", "-c", 'tar -C "$1" -cf - . | zstd -q -o "$2"', "pack", str(stage), str(tb)], check=True)
    (rel_dir / "BUNDLE_MANIFEST.json").write_text(man_text)
    pin = tmp / "fake-obs-release.pin"
    pin.write_text(_pin_text(commit, _sha(tb), _sha(rel_dir / "BUNDLE_MANIFEST.json")))
    env = {"OBS_RELEASE_PIN_FILE": str(pin), "OBS_RELEASE_CACHE": str(tmp / "fake-obs-cache"),
           "OBS_RELEASE_URL": "file://%s" % (tmp / "no-obs-release")}
    return env, _sha(stage / "lib/x86_64-linux-gnu/libobs.so.30")
