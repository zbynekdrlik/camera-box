"""issue 1382 -- the rig-test LEDGER registration must record the TEST painter's real PID.

`rig_test_ledger_register_remote_cmds` (scripts/lib/rig-test-ledger.sh) prints REMOTE bash that
appends one JSONL row to the per-box ledger. `rig-mode.sh test` (`painter_launch_remote`) calls it
with the PID of a painter that only exists on cam2 AFTER the same remote script launched it, so the
PID has to be a REMOTE variable reference that expands on the box. The pre-fix builder spliced that
argument into a SINGLE-quoted printf format and rig-mode.sh passed `\\$PAINTER_PID`: the ledger got
the literal text `\\$PAINTER_PID` (an invalid JSON escape), `event_mode_ledger_cleanup`'s jq read
came back empty and the entry was skipped as malformed -- the painter was never terminated through
the ledger (found 27.9.2026 at the EVENT switch).

Every test here EXECUTES the generated remote text in a local bash against a temp ledger file and
parses the written line as JSON -- never a text match on the builder's source. Tier-0 (#557): no
cargo; the lib and rig-mode.sh are sourced the same way tests/python/test_cam2_painter_wall_clock_1312.py
and tests/harness_rig_test_ledger_723.rs do.
"""

import json
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LEDGER_LIB = _ROOT / "scripts" / "lib" / "rig-test-ledger.sh"
_RIG_MODE = _ROOT / "scripts" / "rig-mode.sh"
_DEFAULT_LEDGER = "/run/camera-box-rig-tests.jsonl"


def _bash(script: str, env_extra: dict | None = None) -> subprocess.CompletedProcess:
    env = {"PATH": "/usr/bin:/bin"}
    env.update(env_extra or {})
    return subprocess.run(["bash", "-c", script], env=env, capture_output=True, text=True)


def _register_text(*args: str) -> str:
    """The remote text the builder prints for ARGS (positional, verbatim -- no shell re-parsing:
    bash -c takes them as "$@" after the $0 placeholder)."""
    proc = subprocess.run(
        ["bash", "-c", 'set -uo pipefail\n. "$LIB"\nrig_test_ledger_register_remote_cmds "$@"', "harness", *args],
        env={"PATH": "/usr/bin:/bin", "LIB": str(_LEDGER_LIB)},
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"builder exited {proc.returncode}: stderr={proc.stderr!r}"
    return proc.stdout


def _run_remote(text: str, env_extra: dict | None = None) -> subprocess.CompletedProcess:
    """Execute the generated REMOTE text the way ssh hands it to the box's shell."""
    proc = _bash(text, env_extra)
    assert proc.returncode == 0, (
        f"remote text exited {proc.returncode}\ntext={text}\nstdout={proc.stdout!r}\nstderr={proc.stderr!r}"
    )
    return proc


def _only_row(ledger: pathlib.Path) -> dict:
    lines = [ln for ln in ledger.read_text().splitlines() if ln.strip()]
    assert len(lines) == 1, f"expected exactly one ledger row, got {lines!r}"
    try:
        return json.loads(lines[0])
    except json.JSONDecodeError as exc:  # the 27.9.2026 symptom: event mode skips it as malformed
        raise AssertionError(f"ledger row is not valid JSON ({exc}): {lines[0]!r}") from exc


def test_register_expands_a_remote_pid_variable_on_the_box_1382(tmp_path):
    ledger = tmp_path / "rig-tests.jsonl"
    text = _register_text(
        "frame-probe --paint-only (rig-mode TEST painter)",
        "$PAINTER_PID",
        "cam2",
        "rig-mode.sh test",
        "7200",
        str(ledger),
    )
    proc = _run_remote(text, {"PAINTER_PID": "4242"})
    row = _only_row(ledger)
    assert row["pid_or_unit"] == "4242", row
    assert row["what"] == "frame-probe --paint-only (rig-mode TEST painter)"
    assert row["box"] == "cam2"
    assert row["started_by"] == "rig-mode.sh test"
    assert row["max_duration_secs"] == 7200
    assert isinstance(row["start_epoch"], int) and row["start_epoch"] > 1_700_000_000
    assert "pid_or_unit=4242" in proc.stdout, proc.stdout


def test_register_passes_a_literal_unit_name_through_unchanged_1382(tmp_path):
    ledger = tmp_path / "rig-tests.jsonl"
    text = _register_text(
        "cam2 painter unit", "cam2-painter.service", "cam2", "rig-mode.sh test", "3600", str(ledger)
    )
    proc = _run_remote(text)
    row = _only_row(ledger)
    assert row["pid_or_unit"] == "cam2-painter.service", row
    assert "pid_or_unit=cam2-painter.service" in proc.stdout, proc.stdout


def test_register_passes_the_recording_e2e_literal_pid_through_unchanged_1382(tmp_path):
    # scripts/recording-e2e.sh resolves the painter PID on dev1 (pgrep over ssh) and passes the
    # NUMBER, with its RUN_ID spliced into `what` -- the other production caller.
    ledger = tmp_path / "rig-tests.jsonl"
    text = _register_text(
        "frame-probe --paint-only (recording-e2e run 1234567890)",
        "31337",
        "cam2",
        "recording-e2e.sh",
        "360",
        str(ledger),
    )
    _run_remote(text)
    row = _only_row(ledger)
    assert row["pid_or_unit"] == "31337", row
    assert row["what"] == "frame-probe --paint-only (recording-e2e run 1234567890)"
    assert row["started_by"] == "recording-e2e.sh"
    assert row["max_duration_secs"] == 360


def test_register_keeps_the_row_valid_json_when_text_needs_escaping_1382(tmp_path):
    # A `what` / `started_by` carrying JSON-special and shell-special characters must land
    # byte-for-byte: never expanded on the box ($HOME, `...`), never a printf directive (%),
    # never an unescaped JSON quote/backslash.
    ledger = tmp_path / "rig-tests.jsonl"
    what = 'painter "quoted" back\\slash 100% $HOME `echo x` it\'s'
    started_by = 'run "a" \\ 50%s $(echo y)'
    text = _register_text(what, "4242", "cam2", started_by, "60", str(ledger))
    _run_remote(text, {"HOME": "/should/not/expand"})
    row = _only_row(ledger)
    assert row["what"] == what, row
    assert row["started_by"] == started_by, row
    assert row["pid_or_unit"] == "4242", row


def _painter_launch_remote(path: str = "/usr/bin:/bin") -> str:
    """The full remote script `rig-mode.sh test` sends to cam2 (built on dev1, never run here)."""
    proc = subprocess.run(
        [
            "bash",
            "-c",
            'set -uo pipefail\n. "$SCRIPT"\n'
            "painter_launch_remote /usr/local/bin/frame-probe 7200 700 /run/rig-painter.pid '' 60 "
            "'' hw:CARD=PCH,DEV=3 180 /run/rig-qpsk-markers.csv",
        ],
        env={"PATH": path, "SCRIPT": str(_RIG_MODE)},
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"painter_launch_remote exited {proc.returncode}: {proc.stderr!r}"
    return proc.stdout


def _rig_mode_register_block() -> str:
    """The ledger-registration lines exactly as `rig-mode.sh test` sends them to cam2."""
    lines = _painter_launch_remote().splitlines()
    start = next(i for i, ln in enumerate(lines) if ln.startswith("PAINTER_PID="))
    end = next(i for i in range(start + 1, len(lines)) if lines[i] == "sleep 3")
    block = "\n".join(ln for ln in lines[start + 1 : end] if not ln.lstrip().startswith("#"))
    assert _DEFAULT_LEDGER in block, f"the TEST painter must register into the default ledger:\n{block}"
    return block


def test_rig_mode_test_painter_registration_records_the_numeric_pid_1382(tmp_path):
    ledger = tmp_path / "rig-tests.jsonl"
    block = _rig_mode_register_block().replace(_DEFAULT_LEDGER, str(ledger))
    proc = _run_remote(block, {"PAINTER_PID": "4242"})
    row = _only_row(ledger)
    assert row["pid_or_unit"] == "4242", row
    assert row["what"] == "frame-probe --paint-only (rig-mode TEST painter)"
    assert row["started_by"] == "rig-mode.sh test"
    assert row["max_duration_secs"] == 7200
    assert "pid_or_unit=4242" in proc.stdout, proc.stdout
    # The EVENT-mode reader (event_mode_ledger_cleanup) reads the row with jq; it must see the PID.
    jq = subprocess.run(
        ["jq", "-r", ".pid_or_unit // empty"], input=ledger.read_text(), capture_output=True, text=True
    )
    assert jq.returncode == 0 and jq.stdout.strip() == "4242", (jq.returncode, jq.stdout, jq.stderr)


def test_painter_launch_remote_runs_nothing_on_dev1_while_building_the_cam2_script_1382(tmp_path):
    # Found next to the ledger call (same unquoted <<REMOTE heredoc): a comment spelling a command
    # in backticks is a COMMAND SUBSTITUTION there, so building the cam2 script ran `fuser` on
    # dev1 (stderr "Specified filename /dev/fb0 does not exist.") and the comment reached cam2
    # with the command text gone. A stub `fuser` first on PATH must never be called by the builder.
    bindir = tmp_path / "bin"
    bindir.mkdir()
    marker = tmp_path / "fuser-ran-locally"
    stub = bindir / "fuser"
    stub.write_text(f"#!/bin/sh\necho \"$*\" >> '{marker}'\nexit 1\n")
    stub.chmod(0o755)
    text = _painter_launch_remote(f"{bindir}:/usr/bin:/bin")
    assert not marker.exists(), f"painter_launch_remote ran fuser on dev1: {marker.read_text()!r}"
    assert "fuser -s /dev/fb0" in text  # the remote checks themselves are still in the script
