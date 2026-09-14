"""#1312 -- the cam2 painter must stamp its QPSK marker CSV on the DanteSync WALL clock.

The `avlatency` development-handover item (rig_dev_handover_decision.py /
scripts/measurement-chain-latency.sh) pairs cam2's painter `emit_ts_ns` markers against dev1's
wall-clock `mbc` meter onsets. `frame-probe --paint-only` defaults to a MONOTONIC
`start.elapsed()` emit clock; only `--wall-clock` (src/bin/frame-probe.rs, stamps `gen_ts_ns` on
CLOCK_REALTIME) makes the emit comparable to the absolute onset clock. So BOTH painters that write
the steady-state marker log `/run/rig-qpsk-markers.csv` -- the PERMANENT `cam2-painter.service`
(scripts/setup-device.sh) and the TRANSIENT rig-mode TEST painter (scripts/rig-mode.sh
`painter_launch_remote`) -- must pass `--wall-clock`. Until then `avlatency` reads UNKNOWN forever
(the monotonic-emit trap, .claude/rules/rig-dev-handover-check.md).

The E2E harness (scripts/recording-e2e.sh) already passes `--wall-clock` on its own transient burn
painter and is deliberately NOT re-tested here.

Tier-0 (#557): no cargo. Each painter argv is obtained by SOURCING the script and invoking its pure
builder in bash -- the SAME `run_sourced` convention tests/rig_mode.rs and
tests/harness_cam2_painter_provisioning_863.rs use on CI, so a green here predicts the Rust
harnesses' pass.
"""

import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SETUP_DEVICE = _ROOT / "scripts" / "setup-device.sh"
_RIG_MODE = _ROOT / "scripts" / "rig-mode.sh"


def _run_sourced(script: pathlib.Path, body: str) -> str:
    """Source `script` (its BASH_SOURCE!=$0 guard skips main) and run `body`, returning stdout.

    Mirrors tests/rig_mode.rs::run_sourced and
    tests/harness_cam2_painter_provisioning_863.rs::run_sourced_setup_device.
    """
    harness = 'set -uo pipefail\n. "$SCRIPT"\n' + body
    proc = subprocess.run(
        ["bash", "-c", harness],
        env={"SCRIPT": str(script), "PATH": "/usr/bin:/bin"},
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, (
        f"sourced harness exited {proc.returncode}\nstdout={proc.stdout!r}\nstderr={proc.stderr!r}"
    )
    return proc.stdout


def _permanent_unit() -> str:
    return _run_sourced(_SETUP_DEVICE, "cam2_painter_service_unit_content")


def _transient_launch() -> str:
    # Same positional args tests/rig_mode.rs::painter_launch drives it with, plus the explicit
    # audio-marker / marker-log tail so the emitted command is the full production shape.
    return _run_sourced(
        _RIG_MODE,
        "painter_launch_remote /usr/local/bin/frame-probe 7200 700 /run/rig-painter.pid '' 60 "
        "'' hw:CARD=PCH,DEV=3 180 /run/rig-qpsk-markers.csv",
    )


def _execstart_line(unit: str) -> str:
    lines = [l for l in unit.splitlines() if l.startswith("ExecStart=")]
    assert len(lines) == 1, f"expected exactly one ExecStart line, got {lines!r}"
    return lines[0]


def test_permanent_unit_execstart_carries_wall_clock():
    line = _execstart_line(_permanent_unit())
    assert "--wall-clock" in line, (
        "#1312: the PERMANENT cam2-painter.service ExecStart must pass --wall-clock so its "
        f"emit_ts_ns is on the DanteSync wall clock (avlatency pairing). Got:\n{line}"
    )


def test_permanent_unit_keeps_every_other_flag_byte_identical():
    line = _execstart_line(_permanent_unit())
    # every pre-existing pinned flag must survive verbatim (only --wall-clock is added)
    for needle in (
        "ExecStart=/usr/local/bin/frame-probe",
        "--paint-only",
        "--dual-qr",
        "--qr-size 700",
        "--paint-fps 60",
        "--duration-secs 31536000",
        "--marker-log /run/rig-qpsk-markers.csv",
    ):
        assert needle in line, f"#1312: pinned flag {needle!r} must stay byte-identical. Got:\n{line}"


def test_transient_rig_mode_painter_carries_wall_clock():
    out = _transient_launch()
    launch = [l for l in out.splitlines() if "nohup" in l and "--paint-only" in l]
    assert len(launch) == 1, f"expected one nohup'd painter launch line, got {launch!r}"
    line = launch[0]
    assert "--wall-clock" in line, (
        "#1312: rig-mode.sh's transient TEST painter (writes the SAME /run/rig-qpsk-markers.csv) "
        f"must pass --wall-clock too, so the marker log is wall-clock in every state. Got:\n{line}"
    )


def test_transient_painter_preserves_the_pinned_vernier_anchor():
    # tests/rig_mode.rs asserts this exact contiguous span; --wall-clock must NOT be inserted
    # inside it (place it after --duration-secs / --paint-fps).
    out = _transient_launch()
    launch = [l for l in out.splitlines() if "nohup" in l and "--paint-only" in l][0]
    assert (
        "/usr/local/bin/frame-probe --paint-only --dual-qr --qr-size 700 --duration-secs 7200"
        in launch
    ), f"#247/#1312: the pinned contiguous vernier anchor must stay intact. Got:\n{launch}"
    assert "--paint-fps 60" in launch, f"#290: --paint-fps 60 must survive. Got:\n{launch}"
