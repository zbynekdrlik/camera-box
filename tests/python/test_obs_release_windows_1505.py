"""Issue 1505: the Windows OBS boxes take the pinned obs release's Windows zip.

`scripts/obs-release.pin` carries the sha256 of `obs-windows-x64-<tag>.zip` and of its
`WINDOWS_MANIFEST.json` beside the strih ones. `obs_release_windows_bundle` proves the zip against the
pin AND the release's own `.sha256` (git-bash `<sha> *<name>` form), the manifest against the pin
(the zip's own `BUNDLE_MANIFEST.json` must be the same bytes), every manifest file in the unzipped
tree, the required files and the markers naming the pinned tag + commit. No fallback: any failure
removes DEST. The deploy planner stages THAT tree for a Windows box instead of the anchor run's
windows-genlock artifact. A fake release is served from file://, so nothing reaches GitHub.
"""
import hashlib
import json
import os
import subprocess
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "obs-release.sh"
DEPLOY = REPO / "scripts" / "deploy-genlock-fleet.sh"
TAG = "v9.9.7"
COMMIT = "abc1505"
ZIP = f"obs-windows-x64-{TAG}.zip"


def _sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def _lib(body, *args, env=None):
    e = {"PATH": "/usr/bin:/bin"}
    e.update(env or {})
    return subprocess.run(["bash", "-c", f"set -euo pipefail\n. '{LIB}'\n{body}", "harness", *args],
                          capture_output=True, text=True, timeout=120, env=e)


WIN_FILES = {
    "bin/64bit/obs64.exe": "obs64 bytes\n",
    "bin/64bit/obs.dll": "libobs bytes\n",
    "obs-plugins/64bit/distroav.dll": "distroav bytes\n",
    "obs-plugins/64bit/obs-vban.dll": "vban bytes\n",
    "obs-plugins/64bit/vlc-video.dll": "vlc bytes\n",
    "obs-plugins/64bit/obs-audio-video-sync-dock.dll": "dock bytes\n",
    "data/obs-plugins/distroav/locale/en-US.ini": "x=1\n",
}


def _release(tmp, tamper=None, zip_manifest="same", side=None, commit=COMMIT, drop=(), zip_extra=None):
    """Publish a fake Windows release under tmp/srv/<tag>/ + its pin; returns the env."""
    stage = tmp / "stage"
    files = dict(WIN_FILES)
    files["GENLOCK_BUILD_SHA.txt"] = commit + "\n"
    files["OBS_RELEASE.txt"] = f"tag={TAG}\ncommit={commit}\nversion={TAG[1:]}\n"
    for rel in drop:
        files.pop(rel)
    for rel, text in files.items():
        p = stage / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)
    man = {"release": {"obs_tag": TAG, "obs_commit": COMMIT, "genlock_stats_version": 4,
                       "genlock_output_stats_version": 1},
           "schema": "camera-box/genlock-bundle-manifest@1",
           "files": [{"path": rel, "sha256": _sha(stage / rel), "size": (stage / rel).stat().st_size}
                     for rel in files]}
    man_text = json.dumps(man, indent=2) + "\n"
    if zip_manifest == "same":
        (stage / "BUNDLE_MANIFEST.json").write_text(man_text)
    elif zip_manifest == "other":
        (stage / "BUNDLE_MANIFEST.json").write_text(man_text.replace("camera-box", "camera-bix"))
    if tamper:
        tamper(stage)
    srv = tmp / "srv" / TAG
    srv.mkdir(parents=True)
    zp = srv / ZIP
    with zipfile.ZipFile(zp, "w") as z:
        for p in sorted(stage.rglob("*")):
            if p.is_file():
                z.write(p, p.relative_to(stage).as_posix())
        if zip_extra:
            zip_extra(z)
    (srv / "WINDOWS_MANIFEST.json").write_text(man_text)
    (srv / (ZIP + ".sha256")).write_text(f"{side or _sha(zp)} *{ZIP}\n")
    pin = tmp / "obs-release.pin"
    pin.write_text(f"OBS_RELEASE_TAG={TAG}\nOBS_RELEASE_COMMIT={COMMIT}\n"
                   f"OBS_RELEASE_STRIH_SHA256={'0' * 64}\nOBS_RELEASE_MANIFEST_SHA256={'0' * 64}\n"
                   f"OBS_RELEASE_WINDOWS_SHA256={_sha(zp)}\n"
                   f"OBS_RELEASE_WINDOWS_MANIFEST_SHA256={_sha(srv / 'WINDOWS_MANIFEST.json')}\n")
    return {"OBS_RELEASE_URL": f"file://{tmp / 'srv'}", "OBS_RELEASE_PIN_FILE": str(pin),
            "OBS_RELEASE_CACHE": str(tmp / "cache")}


def _bundle(tmp, env):
    dest = tmp / "win"
    return _lib('obs_release_windows_bundle "$1" "$2"', str(REPO), str(dest), env=env), dest


# --- the pin ---------------------------------------------------------------------------------------

def test_the_real_pin_carries_both_windows_sha256():
    r = _lib('obs_pin_has_windows "$1"; obs_pin_windows_sha256 "$1"; obs_pin_windows_manifest_sha256 "$1"',
             str(REPO))
    assert r.returncode == 0, r.stderr
    zsha, msha = r.stdout.split()
    assert len(zsha) == 64 and len(msha) == 64


def test_a_pin_without_the_windows_fields_says_so(tmp_path):
    pin = tmp_path / "p.pin"
    pin.write_text(f"OBS_RELEASE_TAG={TAG}\nOBS_RELEASE_COMMIT={COMMIT}\n")
    r = _lib('obs_pin_has_windows "$1"', str(REPO), env={"OBS_RELEASE_PIN_FILE": str(pin)})
    assert r.returncode == 1


def test_a_malformed_windows_sha256_is_a_named_failure(tmp_path):
    pin = tmp_path / "p.pin"
    pin.write_text(f"OBS_RELEASE_TAG={TAG}\nOBS_RELEASE_COMMIT={COMMIT}\nOBS_RELEASE_WINDOWS_SHA256=xyz\n")
    r = _lib('obs_pin_windows_sha256 "$1"', str(REPO), env={"OBS_RELEASE_PIN_FILE": str(pin)})
    assert r.returncode == 1 and "OBS_RELEASE_WINDOWS_SHA256" in r.stderr, r.stderr


# --- the bundle ------------------------------------------------------------------------------------

def test_the_pinned_windows_zip_unzips_verified_and_needs_no_network_the_second_time(tmp_path):
    env = _release(tmp_path)
    r, dest = _bundle(tmp_path, env)
    assert r.returncode == 0, r.stderr
    assert (dest / "bin/64bit/obs.dll").read_text() == "libobs bytes\n"
    assert _sha(dest / "BUNDLE_MANIFEST.json") == _sha(tmp_path / "srv" / TAG / "WINDOWS_MANIFEST.json")
    r = _lib('obs_release_stats_abi "$2"; obs_release_marker_grade "$2" "$1"', str(REPO), str(dest), env=env)
    assert r.returncode == 0, r.stderr
    assert r.stdout.split("\n")[:2] == ["4:1", f"ok {TAG} {COMMIT}"], r.stdout
    # cached: the release can vanish
    for p in (tmp_path / "srv" / TAG).iterdir():
        p.unlink()
    r = _lib('obs_release_windows_bundle "$1" "$2"', str(REPO), str(tmp_path / "win2"), env=env)
    assert r.returncode == 0, r.stderr


def test_a_zip_the_pin_does_not_name_is_refused_and_nothing_stays(tmp_path):
    env = _release(tmp_path)
    pin = Path(env["OBS_RELEASE_PIN_FILE"])
    pin.write_text("\n".join(l if not l.startswith("OBS_RELEASE_WINDOWS_SHA256=")
                             else "OBS_RELEASE_WINDOWS_SHA256=" + "0" * 64
                             for l in pin.read_text().splitlines()) + "\n")
    r, dest = _bundle(tmp_path, env)
    assert r.returncode == 1 and "the pin says" in r.stderr, r.stderr
    assert not dest.exists()
    assert not list((tmp_path / "cache").rglob("*.zip")), "no unproved bytes kept"


def test_a_release_side_sha256_that_disagrees_is_refused(tmp_path):
    env = _release(tmp_path, side="1" * 64)
    r, dest = _bundle(tmp_path, env)
    assert r.returncode == 1 and "its .sha256 says 1111" in r.stderr, r.stderr
    assert not dest.exists()


def test_a_manifest_the_pin_does_not_name_is_refused(tmp_path):
    env = _release(tmp_path)
    (tmp_path / "srv" / TAG / "WINDOWS_MANIFEST.json").write_text("{}\n")
    r, dest = _bundle(tmp_path, env)
    assert r.returncode == 1 and "WINDOWS_MANIFEST.json" in r.stderr, r.stderr
    assert not dest.exists()


def test_a_zip_whose_own_manifest_differs_from_the_pinned_one_is_refused(tmp_path):
    env = _release(tmp_path, zip_manifest="other")
    r, dest = _bundle(tmp_path, env)
    assert r.returncode == 1 and "not the pinned manifest" in r.stderr, r.stderr
    assert not dest.exists()


def test_a_zip_without_its_own_manifest_gets_the_pinned_one(tmp_path):
    env = _release(tmp_path, zip_manifest=None)
    r, dest = _bundle(tmp_path, env)
    assert r.returncode == 0, r.stderr
    assert _sha(dest / "BUNDLE_MANIFEST.json") == _sha(tmp_path / "srv" / TAG / "WINDOWS_MANIFEST.json")


def test_a_file_that_differs_from_the_manifest_is_refused(tmp_path):
    def tamper(stage):
        (stage / "bin/64bit/obs.dll").write_text("another libobs\n")
    env = _release(tmp_path, tamper=tamper)
    r, dest = _bundle(tmp_path, env)
    assert r.returncode == 1 and "bin/64bit/obs.dll sha256" in r.stderr, r.stderr
    assert not dest.exists()


def test_a_marker_naming_another_commit_is_refused(tmp_path):
    env = _release(tmp_path, commit="def0000")
    r, dest = _bundle(tmp_path, env)
    assert r.returncode == 1 and "GENLOCK_BUILD_SHA.txt" in r.stderr, r.stderr
    assert not dest.exists()


def test_a_zip_without_a_required_windows_file_is_refused(tmp_path):
    env = _release(tmp_path, drop=("obs-plugins/64bit/distroav.dll",))
    r, dest = _bundle(tmp_path, env)
    assert r.returncode == 1 and "has no obs-plugins/64bit/distroav.dll" in r.stderr, r.stderr
    assert not dest.exists()


def test_a_zip_carrying_a_symlink_is_refused(tmp_path):
    """The deploy copies the tree onto a Windows box: a symlink entry is never staged (review finding)."""
    def link(z):
        info = zipfile.ZipInfo("obs-plugins/64bit/evil.dll")
        info.create_system = 3
        info.external_attr = (0o120777 << 16)
        z.writestr(info, "/etc/passwd")
    env = _release(tmp_path, zip_extra=link)
    r, dest = _bundle(tmp_path, env)
    assert r.returncode == 1 and "symlink" in r.stderr, r.stderr
    assert not dest.exists()


def test_a_zip_with_a_duplicate_entry_never_waits_on_a_prompt(tmp_path):
    """unzip asks `replace?` on a second copy of a name; the bundle must never read the terminal."""
    def dup(z):
        import warnings
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            z.writestr("bin/64bit/obs.dll", "a second libobs\n")
    env = _release(tmp_path, zip_extra=dup)
    dest = tmp_path / "win"
    r = subprocess.run(["bash", "-c", f"set -euo pipefail\n. '{LIB}'\nobs_release_windows_bundle \"$1\" \"$2\"",
                        "harness", str(REPO), str(dest)], capture_output=True, text=True, timeout=60,
                       env={"PATH": "/usr/bin:/bin", **env}, input="A\n")
    # (tmp_path holds this test's name, so match the lib's own phrase, never a bare word)
    assert r.returncode == 1 and "carries a duplicate entry" in r.stderr, r.stdout + r.stderr
    assert "[y]es, [n]o" not in r.stdout + r.stderr, "unzip must never prompt"
    assert not dest.exists()


def test_an_unreachable_release_is_refused_never_another_build(tmp_path):
    env = _release(tmp_path)
    env["OBS_RELEASE_URL"] = f"file://{tmp_path / 'nowhere'}"
    r, dest = _bundle(tmp_path, env)
    assert r.returncode == 1 and "never another build" in r.stderr, r.stderr
    assert not dest.exists()


# --- the deploy planner ----------------------------------------------------------------------------

def _fake_gh(tmp):
    d = tmp / "fakebin"
    d.mkdir(exist_ok=True)
    gh = d / "gh"
    gh.write_text(f"#!/bin/bash\necho \"gh $*\" >> '{tmp / 'gh.log'}'\nif [ \"$1 $2\" = 'run list' ]; then echo '[]'; fi\nexit 0\n")
    gh.chmod(0o755)
    return d


def _deploy(tmp, env, *args):
    e = dict(os.environ)
    e.update(env)
    e["HOME"] = str(tmp / "home")
    e["PATH"] = f"{_fake_gh(tmp)}:{e['PATH']}"
    return subprocess.run(["bash", str(DEPLOY), *args], capture_output=True, text=True, timeout=300, env=e)


def test_plan_stages_the_pinned_zip_for_resolume_with_no_anchor_run(tmp_path):
    env = _release(tmp_path)
    r, tree = _bundle(tmp_path, env)
    assert r.returncode == 0, r.stderr
    r = _deploy(tmp_path, env, "--plan", "--stage", str(tree), "--boxes", "resolume-snv")
    assert r.returncode == 0, r.stdout + r.stderr
    out = r.stdout
    assert f"Write-MarkerAtomic (Join-Path $obsDir 'GENLOCK_BUILD_SHA.txt')  '{COMMIT}'" in out
    assert f"Write-MarkerAtomic (Join-Path $obsDir 'DISTROAV_BUILD_SHA.txt') '{COMMIT}'" in out
    assert "@('4', 'output_stats=1')" in out, "the stats pair comes from the pinned manifest"
    assert ZIP in out, "STEP 0 names the pinned zip, not the windows-genlock artifact"
    assert "obs-genlock-windows-x64" not in out
    assert f"C:\\stage-genlock-{COMMIT}" in out


def test_plan_with_a_stage_of_another_release_records_no_stats_pair(tmp_path):
    env = _release(tmp_path)
    r, tree = _bundle(tmp_path, env)
    assert r.returncode == 0, r.stderr
    m = json.loads((tree / "BUNDLE_MANIFEST.json").read_text())
    m["release"]["obs_commit"] = "fff9999"
    (tree / "BUNDLE_MANIFEST.json").write_text(json.dumps(m))
    r = _deploy(tmp_path, env, "--plan", "--stage", str(tree), "--boxes", "resolume-snv")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "@('4', 'output_stats=1')" not in r.stdout
    assert "stats ABI unknown at plan time" in r.stdout


def test_a_fast_deploy_of_a_pinned_windows_box_is_refused(tmp_path):
    env = _release(tmp_path)
    r = _deploy(tmp_path, env, "--plan", "--fast", "--stage", str(tmp_path), "--boxes", "resolume-snv")
    assert r.returncode == 2, r.stdout + r.stderr
    assert "--fast" in r.stderr and "pinned obs release" in r.stderr, r.stderr
    r = _deploy(tmp_path, env, "--plan", "--fast", "--run-id", "R", "--sha", "ab12cd3", "--stage", str(tmp_path),
                "--boxes", "imag,resolume-snv")
    assert r.returncode == 2 and "separately" in r.stderr, r.stdout + r.stderr


def test_a_mixed_imag_and_resolume_plan_gives_each_box_its_own_sha_and_pair(tmp_path):
    """imag keeps the anchor SHA + its obs.h pair; resolume takes the pin commit + the pinned manifest's pair."""
    env = _release(tmp_path)
    r, tree = _bundle(tmp_path, env)
    assert r.returncode == 0, r.stderr
    r = _deploy(tmp_path, env, "--plan", "--run-id", "R", "--sha", "ab12cd3", "--stage", str(tree),
                "--boxes", "imag,resolume-snv")
    assert r.returncode == 0, r.stdout + r.stderr
    out = r.stdout
    imag = out[out.index("box=imag"):out.index("box=resolume-snv")]
    win = out[out.index("box=resolume-snv"):out.index("# fleet-deploy log line")]
    log = out[out.index("# fleet-deploy log line"):]
    assert "\tR\tab12cd3\timag\tfull" in log and f"\tobs-{TAG}\t{COMMIT}\tresolume-snv\tfull" in log, log
    assert "genlock_write_markers \"$MARKER_DIR\" 'ab12cd3' 'ab12cd3' '' ''" in imag, imag
    assert "4:1" not in imag and COMMIT not in imag
    assert f"'GENLOCK_BUILD_SHA.txt')  '{COMMIT}'" in win and f"C:\\stage-genlock-{COMMIT}" in win
    assert "@('4', 'output_stats=1')" in win and ZIP in win and "ab12cd3" not in win
    note = f"Windows boxes resolume-snv take the pinned obs release {TAG} ({COMMIT})"
    assert note in out, "the plan says the Windows box ignores the anchor run"


def test_execute_warns_when_the_verified_manifest_records_no_stats_pair(tmp_path):
    env = _release(tmp_path)
    srv = tmp_path / "srv" / TAG
    # rebuild the release with a manifest whose release block carries no stats versions
    man = json.loads((srv / "WINDOWS_MANIFEST.json").read_text())
    del man["release"]["genlock_stats_version"]
    text = json.dumps(man, indent=2) + "\n"
    (srv / "WINDOWS_MANIFEST.json").write_text(text)
    with zipfile.ZipFile(srv / ZIP, "a") as z:
        import warnings
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            z.writestr("BUNDLE_MANIFEST.json", text)
    # a zip with two BUNDLE_MANIFEST.json entries is refused, so rebuild it cleanly instead
    stage = tmp_path / "stage"
    (stage / "BUNDLE_MANIFEST.json").write_text(text)
    (srv / ZIP).unlink()
    with zipfile.ZipFile(srv / ZIP, "w") as z:
        for p in sorted(stage.rglob("*")):
            if p.is_file():
                z.write(p, p.relative_to(stage).as_posix())
    (srv / (ZIP + ".sha256")).write_text(f"{_sha(srv / ZIP)} *{ZIP}\n")
    pin = Path(env["OBS_RELEASE_PIN_FILE"])
    lines = []
    for l in pin.read_text().splitlines():
        if l.startswith("OBS_RELEASE_WINDOWS_SHA256="):
            l = "OBS_RELEASE_WINDOWS_SHA256=" + _sha(srv / ZIP)
        elif l.startswith("OBS_RELEASE_WINDOWS_MANIFEST_SHA256="):
            l = "OBS_RELEASE_WINDOWS_MANIFEST_SHA256=" + _sha(srv / "WINDOWS_MANIFEST.json")
        lines.append(l)
    pin.write_text("\n".join(lines) + "\n")
    r = _deploy(tmp_path, env, "--boxes", "resolume-snv")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "records no stats ABI pair" in r.stderr, r.stderr
    assert "stats ABI unknown at plan time" in r.stdout


def test_execute_stages_the_verified_pinned_zip_and_never_asks_for_a_windows_genlock_run(tmp_path):
    env = _release(tmp_path)
    r = _deploy(tmp_path, env, "--boxes", "resolume-snv")
    assert r.returncode == 0, r.stdout + r.stderr
    gh_log = (tmp_path / "gh.log").read_text() if (tmp_path / "gh.log").exists() else ""
    assert "windows-genlock" not in gh_log and "run download" not in gh_log, gh_log
    assert f"'GENLOCK_BUILD_SHA.txt')  '{COMMIT}'" in r.stdout
    assert "@('4', 'output_stats=1')" in r.stdout
    log = (tmp_path / "home" / ".camera-box" / "genlock-fleet-deploy.log").read_text()
    fields = log.strip().split("\t")
    assert fields[1:5] == [f"obs-{TAG}", COMMIT, "resolume-snv", "full"], log


def test_execute_refuses_a_pinned_zip_that_does_not_verify_and_prints_no_program(tmp_path):
    env = _release(tmp_path, side="2" * 64)
    r = _deploy(tmp_path, env, "--boxes", "resolume-snv")
    assert r.returncode == 3, r.stdout + r.stderr
    assert "build_windows" not in r.stdout and "Write-MarkerAtomic" not in r.stdout
    gh_log = (tmp_path / "gh.log").read_text() if (tmp_path / "gh.log").exists() else ""
    assert "windows-genlock" not in gh_log, "no fallback to the vendor build"


def test_a_pin_without_the_windows_zip_keeps_the_windows_genlock_artifact(tmp_path):
    env = _release(tmp_path)
    pin = Path(env["OBS_RELEASE_PIN_FILE"])
    pin.write_text("".join(l + "\n" for l in pin.read_text().splitlines() if "WINDOWS" not in l))
    r = _deploy(tmp_path, env, "--boxes", "resolume-snv")
    assert r.returncode == 2 and "--run-id is required" in r.stderr, r.stdout + r.stderr
