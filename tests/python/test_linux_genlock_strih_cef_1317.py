"""issue 1317 — CEF wired into the strih Linux genlock build.

Two groups, both Tier-0 (python + bash, zero cargo):

1. Workflow assertions: the strih job `linux-genlock-build-strih` flips browser ON, fetches the
   EXACT obs-deps CEF the vendored OBS pins (version+sha256 from vendor/obs-studio/CMakePresets.json),
   sha256-verifies it, caches it keyed on the CEF version, and passes CEF_ROOT_DIR to cmake — while
   the imag-parity job `linux-genlock-build` stays browser OFF.

2. verify-strih.sh browser-bundle predicates (sourced from scripts/lib/strih-provision.sh) over a
   fixture install root: a BROWSER-ON bundle must carry obs-browser.so AND libcef.so, present/missing.
"""
import json
import pathlib
import subprocess
import sys

import yaml

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_WF = _ROOT / ".github" / "workflows" / "linux-genlock.yml"
_PRESETS = _ROOT / "vendor" / "obs-studio" / "CMakePresets.json"
_LIB = _ROOT / "scripts" / "lib" / "strih-provision.sh"

CEF_VERSION = "6533"
CEF_UBUNTU_X64_SHA256 = "7963335519a19ccdc5233f7334c5ab023026e2f3e9a0cc417007c09d86608146"
STRIH_JOB = "linux-genlock-build-strih"
IMAG_JOB = "linux-genlock-build"


def _wf_text():
    return _WF.read_text()


def _wf_jobs():
    return yaml.safe_load(_wf_text())["jobs"]


def _job_run_text(job):
    """Concatenate every step's `run` (and its `with` values) so substring checks are step-scoped."""
    parts = []
    for step in job.get("steps", []):
        if isinstance(step.get("run"), str):
            parts.append(step["run"])
        with_ = step.get("with")
        if isinstance(with_, dict):
            for v in with_.values():
                parts.append(str(v))
    return "\n".join(parts)


def _step_run_containing(job, needle):
    """Return the `run` of the FIRST step whose run text contains `needle` (or None)."""
    for step in job.get("steps", []):
        run = step.get("run")
        if isinstance(run, str) and needle in run:
            return run
    return None


# ---- group 1: the workflow ---------------------------------------------------------------------

def test_strih_job_enables_browser():
    jobs = _wf_jobs()
    assert STRIH_JOB in jobs, f"{STRIH_JOB} job missing"
    env = jobs[STRIH_JOB].get("env", {})
    assert str(env.get("STRIH_ENABLE_BROWSER")) == "ON", (
        "1317: the strih job must set STRIH_ENABLE_BROWSER: ON so obs-browser + CEF are built"
    )


def test_strih_job_pins_cef_in_env():
    # Single-source: the version + sha256 + archive pin live ONCE in the job env block; the steps
    # reference ${{ env.CEF_* }}. Assert the pin is present AND exact (the vendored ubuntu-x86_64 pin).
    env = _wf_jobs()[STRIH_JOB].get("env", {})
    assert str(env.get("CEF_VERSION")) == CEF_VERSION, "1317: strih job must pin CEF_VERSION=6533"
    assert env.get("CEF_SHA256") == CEF_UBUNTU_X64_SHA256, (
        "1317: strih job must pin CEF_SHA256 to the vendored ubuntu-x86_64 sha256"
    )
    assert f"cef_binary_{CEF_VERSION}_linux_x86_64" in str(env.get("CEF_ARCHIVE", "")), (
        "1317: strih job must pin CEF_ARCHIVE to the linux x86_64 CEF archive"
    )


def test_strih_job_cef_fetch_verifies_sha256():
    # The fetch step must download the pinned archive and sha256-verify it (referencing the env pin).
    run = _job_run_text(_wf_jobs()[STRIH_JOB])
    assert "sha256sum" in run, "1317: the CEF fetch step must sha256-verify the download"
    assert "CEF_SHA256" in run, "1317: the verify must use the pinned CEF_SHA256 env var"
    assert "CEF_ARCHIVE" in run and "CEF_BASE_URL" in run, (
        "1317: the fetch must download the pinned CEF_ARCHIVE from CEF_BASE_URL"
    )


def test_strih_job_caches_cef_keyed_on_version():
    jobs = _wf_jobs()
    cache_steps = [
        s for s in jobs[STRIH_JOB].get("steps", [])
        if isinstance(s.get("uses"), str) and s["uses"].startswith("actions/cache")
    ]
    assert cache_steps, "1317: the strih job must cache the CEF tarball (actions/cache)"
    keys = " ".join(str(s.get("with", {}).get("key", "")) for s in cache_steps)
    # Keyed on the CEF version (single-source via the env var expansion), so a version bump busts it.
    assert "CEF_VERSION" in keys or CEF_VERSION in keys, (
        "1317: the CEF cache key must be keyed on the CEF version"
    )


def test_strih_job_passes_cef_root_dir_to_cmake():
    # Scoped to the OBS Configure step (the load-bearing arg) — NOT the whole job, where the fetch
    # step's CEF_ROOT_DIR=$GITHUB_ENV export would satisfy a job-wide substring even if the cmake
    # flag were deleted (review finding 1).
    # Target the OBS configure step unambiguously (OBS_VERSION_OVERRIDE is only in it — 'ubuntu-ci'
    # alone is a prefix of the DistroAV step's 'ubuntu-ci-x86_64').
    configure = _step_run_containing(_wf_jobs()[STRIH_JOB], "OBS_VERSION_OVERRIDE")
    assert configure is not None, "1317: the strih job must have an OBS configure step"
    assert "-DCEF_ROOT_DIR" in configure, (
        "1317: the strih OBS configure must pass -DCEF_ROOT_DIR pointing at the extracted CEF dir"
    )


def test_strih_marker_is_truthful_browser_on():
    # The staged STRIH_BUILD_FLAGS.txt must be able to say BROWSER-ON (obs-browser + CEF) — the OFF
    # text stays so the flip is one env change.
    assert "BROWSER-ON" in _wf_text(), (
        "1317: the STRIH_BUILD_FLAGS.txt marker must emit BROWSER-ON when the browser is built"
    )


def test_imag_parity_job_stays_browser_off():
    jobs = _wf_jobs()
    run = _job_run_text(jobs[IMAG_JOB])
    assert "-DENABLE_BROWSER=OFF" in run, (
        "1317: the imag-parity full bundle job must stay browser OFF (imag needs no browser source)"
    )
    env = jobs[IMAG_JOB].get("env", {})
    assert "STRIH_ENABLE_BROWSER" not in env, "1317: the imag job must not gain the strih browser env"


def test_workflow_cef_hash_matches_vendored_pin():
    """Guard against pin drift: the hash hardcoded in the workflow must equal the vendored OBS pin."""
    presets = json.loads(_PRESETS.read_text())
    cef = None
    for p in presets["configurePresets"]:
        if p.get("name") == "dependencies":
            cef = p["vendor"]["obsproject.com/obs-studio"]["dependencies"]["cef"]
            break
    assert cef is not None, "vendored CMakePresets.json dependencies.cef not found"
    assert cef["version"] == CEF_VERSION
    assert cef["hashes"]["ubuntu-x86_64"] == CEF_UBUNTU_X64_SHA256
    assert CEF_UBUNTU_X64_SHA256 in _wf_text(), "workflow CEF hash drifted from the vendored pin"
    # The archive's revision suffix (_v<rev>) must track the vendored revision.ubuntu-x86_64 pin — a
    # bump to _v7 at the same 6533 version would otherwise 404 at CI (review finding 2).
    rev = cef["revision"]["ubuntu-x86_64"]
    archive = str(_wf_jobs()[STRIH_JOB]["env"]["CEF_ARCHIVE"])
    assert f"_v{rev}." in archive, (
        f"1317: CEF_ARCHIVE ({archive}) must carry the vendored revision suffix _v{rev}"
    )


# ---- group 2: verify-strih browser-bundle predicates (sourced bash over a fixture root) ---------

def _source(func_call, *, stdin="", extra=""):
    script = f'{extra}\nsource "{_LIB}"\n{func_call}'
    return subprocess.run(["bash", "-c", script], input=stdin.encode(),
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def test_browser_bundle_required_reads_the_marker():
    assert _source('strih_lx_browser_bundle_required "BROWSER-ON: obs-browser + CEF 6533"').returncode == 0
    assert _source('strih_lx_browser_bundle_required "BROWSER-OFF: obs-browser/CEF is the first follow-up"').returncode == 1
    assert _source('strih_lx_browser_bundle_required ""').returncode == 1


def test_browser_bundle_ok_present_and_missing(tmp_path):
    # present: a fixture install root carrying both obs-browser.so and the CEF runtime libcef.so.
    root = tmp_path / "obs-genlock"
    plug = root / "lib" / "obs-plugins"
    plug.mkdir(parents=True)
    (plug / "obs-browser.so").write_text("")
    (plug / "libcef.so").write_text("")
    find_cmd = (
        f'find "{root}" -type f \\( -name obs-browser.so -o -name libcef.so \\) '
        f'| strih_lx_browser_bundle_ok'
    )
    assert _source(find_cmd).returncode == 0, "present bundle (obs-browser.so + libcef.so) must pass"

    # missing CEF runtime: obs-browser.so alone must FAIL.
    root2 = tmp_path / "obs-genlock-nocef"
    plug2 = root2 / "lib" / "obs-plugins"
    plug2.mkdir(parents=True)
    (plug2 / "obs-browser.so").write_text("")
    find_cmd2 = (
        f'find "{root2}" -type f \\( -name obs-browser.so -o -name libcef.so \\) '
        f'| strih_lx_browser_bundle_ok'
    )
    assert _source(find_cmd2).returncode != 0, "missing libcef.so must fail the bundle check"

    # missing obs-browser.so: libcef.so alone must also FAIL (review finding 5).
    root3 = tmp_path / "obs-genlock-nobrowser"
    plug3 = root3 / "lib" / "obs-plugins"
    plug3.mkdir(parents=True)
    (plug3 / "libcef.so").write_text("")
    find_cmd3 = (
        f'find "{root3}" -type f \\( -name obs-browser.so -o -name libcef.so \\) '
        f'| strih_lx_browser_bundle_ok'
    )
    assert _source(find_cmd3).returncode != 0, "missing obs-browser.so must fail the bundle check"
