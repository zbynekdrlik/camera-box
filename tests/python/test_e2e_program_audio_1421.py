"""issue 1421 (owner 13.10.2026: CG/band songs reached the stream program next to the measurement
signal, and nothing in the E2E refused it): the full-path E2E reads the stream program-audio guard
(scripts/program_audio_guard.py, the strih-snv sampler's exit-code contract) before it touches the
rig and every few seconds while it records, and fails closed naming the audible stream OBS inputs.
The lib runs under the caller's `set -euo pipefail` (recording-e2e.sh's own mode)."""
import os
import subprocess
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib" / "e2e-program-audio.sh"
E2E = REPO / "scripts" / "recording-e2e.sh"


def _fakes(tmp_path, codes):
    """A fake guard that answers the next exit code from `codes` per call (the last one repeats),
    and a fake inventory that prints a marker line."""
    seq = tmp_path / "codes"
    seq.write_text("\n".join(str(c) for c in codes) + "\n")
    calls = tmp_path / "calls"
    guard = tmp_path / "guard"
    guard.write_text(
        "#!/usr/bin/env bash\n"
        f'n=$(wc -l < "{calls}" 2>/dev/null || echo 0)\n'
        f'echo x >> "{calls}"\n'
        f'c=$(sed -n "$((n + 1))p" "{seq}"); [ -n "$c" ] || c=$(tail -n 1 "{seq}")\n'
        'case "$c" in 0) v=MEASUREMENT;; 1) v=FOREIGN;; *) v=UNKNOWN;; esac\n'
        'echo "program-audio verdict=$v rms=-20.0 outside_band=55.0% age=1.0 markers=8 chain=8"\n'
        'exit "$c"\n')
    inv = tmp_path / "inventory"
    inv.write_text("#!/usr/bin/env bash\necho \"INVENTORY host=$1\"\n")
    for f in (guard, inv):
        f.chmod(0o755)
    env = dict(os.environ, E2E_PROGRAM_AUDIO_GUARD_CMD=str(guard), E2E_PROGRAM_AUDIO_INVENTORY_CMD=str(inv),
               E2E_PROGRAM_AUDIO_POLL_S="0.2")
    return env, calls


def _bash(script, env, *args, timeout=60):
    return subprocess.run(["bash", "-c", "set -euo pipefail\n. \"$1\"\nshift\n" + script, "h", str(LIB), *args],
                          capture_output=True, text=True, env=env, timeout=timeout)


def test_preflight_passes_on_a_clean_program(tmp_path):
    env, _ = _fakes(tmp_path, [0])
    r = _bash('e2e_program_audio_preflight "$1" stream-host; echo AFTER', env, str(REPO / "scripts"))
    assert r.returncode == 0, r.stderr
    assert "verdict=MEASUREMENT" in r.stdout and "AFTER" in r.stdout
    assert "INVENTORY" not in r.stdout


def test_preflight_refuses_foreign_audio_and_names_the_inputs(tmp_path):
    env, _ = _fakes(tmp_path, [1])
    r = _bash('if e2e_program_audio_preflight "$1" stream-host; then echo PASSED; else echo "RC=$?"; fi',
              env, str(REPO / "scripts"))
    assert "PASSED" not in r.stdout and "RC=1" in r.stdout
    out = r.stdout + r.stderr
    assert "verdict=FOREIGN" in out and "INVENTORY host=stream-host" in out


def test_preflight_refuses_an_unknown_reading(tmp_path):
    env, _ = _fakes(tmp_path, [2])
    r = _bash('if e2e_program_audio_preflight "$1" stream-host; then echo PASSED; else echo "RC=$?"; fi',
              env, str(REPO / "scripts"))
    assert "RC=1" in r.stdout, r.stdout + r.stderr


def test_watch_catches_one_foreign_poll_in_the_recording_window(tmp_path):
    env, calls = _fakes(tmp_path, [0, 0, 1, 0])
    d = tmp_path / "run"
    d.mkdir()
    r = _bash('e2e_program_audio_watch_start "$1" "$2"; sleep 1.5\n'
              'if e2e_program_audio_watch_stop "$1" stream-host "$2"; then echo PASSED; else echo "RC=$?"; fi',
              env, str(REPO / "scripts"), str(d))
    assert "PASSED" not in r.stdout and "RC=1" in r.stdout, r.stdout + r.stderr
    out = r.stdout + r.stderr
    assert "verdict=FOREIGN" in out and "INVENTORY host=stream-host" in out
    assert len(calls.read_text().split()) >= 4  # the polls ran, then the final read


def test_watch_passes_a_clean_recording_window(tmp_path):
    env, _ = _fakes(tmp_path, [0])
    d = tmp_path / "run"
    d.mkdir()
    r = _bash('e2e_program_audio_watch_start "$1" "$2"; sleep 0.7\n'
              'e2e_program_audio_watch_stop "$1" stream-host "$2"; echo AFTER', env, str(REPO / "scripts"), str(d))
    assert r.returncode == 0, r.stdout + r.stderr
    assert "AFTER" in r.stdout and "INVENTORY" not in r.stdout


def test_watch_fails_on_the_final_read_alone(tmp_path):
    # the loop never polls (huge interval after its first read); only the final read is FOREIGN
    env, _ = _fakes(tmp_path, [0, 1])
    env["E2E_PROGRAM_AUDIO_POLL_S"] = "30"
    d = tmp_path / "run"
    d.mkdir()
    r = _bash('e2e_program_audio_watch_start "$1" "$2"; sleep 0.5\n'
              'if e2e_program_audio_watch_stop "$1" stream-host "$2"; then echo PASSED; else echo "RC=$?"; fi',
              env, str(REPO / "scripts"), str(d))
    assert "RC=1" in r.stdout, r.stdout + r.stderr


def test_watch_stop_actually_ends_the_loop(tmp_path):
    env, calls = _fakes(tmp_path, [0])
    d = tmp_path / "run"
    d.mkdir()
    r = _bash('e2e_program_audio_watch_start "$1" "$2"; sleep 0.5\n'
              'e2e_program_audio_watch_stop "$1" stream-host "$2"', env, str(REPO / "scripts"), str(d))
    assert r.returncode == 0, r.stderr
    n = len(calls.read_text().split())
    time.sleep(1.0)
    assert len(calls.read_text().split()) == n, "the poll loop kept running after stop"


def test_the_loop_ends_when_the_e2e_process_is_gone(tmp_path):
    env, calls = _fakes(tmp_path, [0])
    d = tmp_path / "run"
    d.mkdir()
    # the caller exits without stop (an abort): the loop must notice and end by itself
    r = _bash('e2e_program_audio_watch_start "$1" "$2"; sleep 0.3; exit 0', env, str(REPO / "scripts"), str(d))
    assert r.returncode == 0, r.stderr
    time.sleep(1.0)
    n = len(calls.read_text().split())
    time.sleep(1.0)
    assert len(calls.read_text().split()) == n, "the poll loop outlived the E2E process"


def test_the_e2e_wires_preflight_watch_and_stop():
    s = E2E.read_text()
    assert s.count('. "$HERE/lib/e2e-program-audio.sh"') == 1
    pre = s.find('e2e_program_audio_preflight "$HERE" "$STREAM" || exit 1')
    assert pre > 0
    # before the first rig mutation (the colour-control apply) and before any StartRecord
    assert pre < s.find('echo "[0/8] apply device-own-default colour controls')
    start = s.find('e2e_program_audio_watch_start "$HERE" "$OUTDIR"')
    stream_started = s.find('STREAM_RECORDING_STARTED=1  # #649')
    assert stream_started > 0 and start > stream_started
    assert start < s.find('echo "[6/8]')
    stop = s.find('e2e_program_audio_watch_stop "$HERE" "$STREAM" "$OUTDIR" || exit 1')
    assert s.find('echo "[7/8] StopRecord') < stop
    assert stop < s.find('STRIH_HOST_PATH=$(python3 "$HERE/obs_phase2.py" record --host "$STRIH"  --action stop)')


def test_the_inventory_script_exists_and_is_read_only():
    inv = (REPO / "scripts" / "stream_audio_inventory.py").read_text()
    for banned in ("SetInputMute", "SetCurrentProgramScene", "SetInputSettings", "StartRecord", "StopRecord"):
        assert banned not in inv, banned
    assert "InputVolumeMeters" in inv and "GetInputMute" in inv
