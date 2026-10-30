"""Issue 1387 (30.10.2026): the [0/8] auto-aligns source their candidate binaries from the commit's
ci.yml run while that run may still be running.

The camera-box-linux-amd64 and probe-tools-linux-amd64 artifacts are uploaded by ci.yml's `Build`
job a few minutes in; the whole run takes 30+ minutes (the Python harness tests). Since the E2E's
own artifact fetch waits only for the windows-probe job, the harness reaches [0/8] long before the
run concludes, and an align that asks for `--status success` finds nothing (E2E run 41001425945:
"no successful ci.yml run for commit ... to source probe-tools-linux-amd64"). Both aligns resolve
the run through `ci_build_job_run_id`: the commit's ci.yml run in any state, accepted once its
`Build` job completed with success, refused while it runs or after it failed.
"""

import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LIB = ROOT / "scripts/lib/ci-build-job-run.sh"
CAMBOX = ROOT / "scripts/lib/camera-box-parity-align.sh"
FPROBE = ROOT / "scripts/lib/frame-probe-parity-align.sh"

SHA = "0123456789abcdef0123456789abcdef01234567"


def _fake_gh(tmp: Path, build_state: str) -> Path:
    """A gh that knows ONE ci.yml run (4242) for SHA, still in progress, whose Build job is in
    `build_state` ("completed success" / "completed failure" / "in_progress "). `--status success`
    finds nothing, as on the real API while the run is running. Every call is logged."""
    bindir = tmp / "bin"
    bindir.mkdir()
    log = tmp / "gh.log"
    gh = bindir / "gh"
    status, _, conclusion = build_state.partition(" ")
    gh.write_text(
        "#!/usr/bin/env bash\n"
        f'printf "%s\\n" "$*" >> "{log}"\n'
        'case "$*" in\n'
        '  *"run list"*"--status success"*) exit 0 ;;\n'
        f'  *"run list"*"--commit {SHA}"*) echo 4242; exit 0 ;;\n'
        '  *"api"*"runs/4242/jobs"*)\n'
        f'    echo "{status} {conclusion}"; exit 0 ;;\n'
        '  *"run download"*) exit 1 ;;\n'
        "esac\n"
        "exit 0\n"
    )
    gh.chmod(0o755)
    return bindir


def _env(bindir: Path) -> dict:
    return dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}")


def _bash(script: str, bindir: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", "-c", "set -euo pipefail\n" + script, "harness", *args],
        env=_env(bindir), capture_output=True, text=True, timeout=60,
    )


def test_the_helper_takes_a_running_ci_run_whose_build_job_succeeded(tmp_path):
    bindir = _fake_gh(tmp_path, "completed success")
    out = _bash('. "$1"; ci_build_job_run_id repo/x "$2"', bindir, str(LIB), SHA)
    assert out.returncode == 0, out.stderr
    assert out.stdout.strip() == "4242"


def test_the_helper_refuses_while_the_build_job_runs_or_after_it_failed(tmp_path):
    for i, state in enumerate(["in_progress ", "completed failure", "completed cancelled"]):
        d = tmp_path / str(i)
        d.mkdir()
        bindir = _fake_gh(d, state)
        out = _bash('. "$1"; ci_build_job_run_id repo/x "$2"', bindir, str(LIB), SHA)
        assert out.returncode == 0, out.stderr
        assert out.stdout.strip() == "", (state, out.stdout)


def test_the_frame_probe_align_downloads_from_the_running_ci_run(tmp_path):
    bindir = _fake_gh(tmp_path, "completed success")
    out = _bash(
        '. "$1"; FRAME_PROBE_ALIGN_CANDIDATE_SHA="$2" frame_probe_align_resolve_ci_bin >/dev/null',
        bindir, str(FPROBE), SHA,
    )
    log = (tmp_path / "gh.log").read_text()
    assert "run download 4242" in log, (log, out.stderr)
    assert "--status success" not in log, log


def test_the_camera_box_align_downloads_from_the_running_ci_run(tmp_path):
    bindir = _fake_gh(tmp_path, "completed success")
    _bash(
        '. "$1"; CAMBOX_ALIGN_CANDIDATE_SHA="$2" cambox_align_deploy 1.8.1 cam3 || true',
        bindir, str(CAMBOX), SHA,
    )
    log = (tmp_path / "gh.log").read_text()
    assert "run download 4242" in log, log
    assert "--status success" not in log, log
