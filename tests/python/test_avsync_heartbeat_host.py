"""#1331 -- unit tests for the heartbeat SOURCE selection added to scripts/lib/avsync-heartbeat.sh.

The lipsync measurement moved OFF the stream box onto dev2 (GPU); the heartbeat CONTRACT is
unchanged, only the HOST + transport + remote read command differ. `AVSYNC_HEARTBEAT_HOST` selects
"dev2" (default: Linux box, key auth, plain ssh, `cat`) or "stream" (retired Windows box, sshpass,
cmd.exe `type`). These tests pin the probe-cmd shapes, the dev2 default, that the dev2 path carries
NO password, and the stream fallback -- via the SAME `run_sourced` bash-sourcing convention
tests/rig_mode.rs / test_cam2_painter_wall_clock_1312.py use.
"""

import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_HOME = str(pathlib.Path.home())
_LIB = _ROOT / "scripts" / "lib" / "avsync-heartbeat.sh"
_SEP = "---AVSYNC-HB-SEP---"


def _run(body: str, env_extra: dict | None = None) -> str:
    """Source avsync-heartbeat.sh (its functions only -- it is a pure lib) and run `body`."""
    env = {"PATH": "/usr/bin:/bin", "HOME": _HOME, "SCRIPT": str(_LIB)}
    if env_extra:
        env.update(env_extra)
    harness = 'set -uo pipefail\n. "$SCRIPT"\n' + body
    proc = subprocess.run(
        ["bash", "-c", harness], env=env, capture_output=True, text=True
    )
    assert proc.returncode == 0, (
        f"harness exited {proc.returncode}\nstdout={proc.stdout!r}\nstderr={proc.stderr!r}"
    )
    return proc.stdout


# ── host selection default + override ────────────────────────────────────────


def test_default_host_is_dev2():
    assert _run("avsync_heartbeat_host").strip() == "dev2"


def test_host_override_stream():
    assert _run("avsync_heartbeat_host", {"AVSYNC_HEARTBEAT_HOST": "stream"}).strip() == "stream"


def test_host_override_dev2_explicit():
    assert _run("avsync_heartbeat_host", {"AVSYNC_HEARTBEAT_HOST": "dev2"}).strip() == "dev2"


# ── remote read command shapes ───────────────────────────────────────────────


def test_dev2_remote_cmd_uses_cat_and_linux_path_and_separator():
    out = _run("avsync_heartbeat_remote_cmd dev2")
    assert "cat " in out
    assert ".camera-box/avsync-watchdog-heartbeat.txt" in out
    assert _SEP in out
    # dev2 is Linux -- never the Windows `type`/drive-path shape.
    assert "type " not in out
    assert "C:\\" not in out
    assert "2>/dev/null" in out


def test_dev2_remote_cmd_leaves_home_literal_for_remote_expansion():
    # The whole cmd string is passed verbatim through `ssh host "$cmd"`; $HOME must reach the REMOTE
    # shell (dev2) as a literal, never be expanded locally.
    out = _run("avsync_heartbeat_remote_cmd dev2")
    assert "$HOME/.camera-box/avsync-watchdog-heartbeat.txt" in out


def test_dev2_remote_cmd_has_no_vlc_file():
    # dev2 has no VLC-monitor heartbeat; only the watchdog file + separator are emitted.
    out = _run("avsync_heartbeat_remote_cmd dev2")
    assert "vlc" not in out.lower()


def test_stream_remote_cmd_is_the_windows_type_probe():
    out = _run("avsync_heartbeat_remote_cmd stream")
    assert "type " in out
    assert "C:\\avsync\\avsync-watchdog-heartbeat.txt" in out
    assert _SEP in out
    assert "2>nul" in out


def test_default_remote_cmd_matches_dev2():
    assert _run("avsync_heartbeat_remote_cmd") == _run("avsync_heartbeat_remote_cmd dev2")


# ── ssh transport prefix: NO password on the dev2 path ───────────────────────


def test_dev2_ssh_prefix_is_keyauth_no_password():
    out = _run("avsync_heartbeat_ssh_prefix_argv dev2")
    lines = [l for l in out.splitlines() if l != ""]
    assert lines[0] == "ssh", f"dev2 must use plain ssh (key auth), got {lines!r}"
    assert "newlevel@dev2" in lines
    # THE point of the dev2 path: no sshpass, no password anywhere in the argv.
    assert "sshpass" not in out
    assert "-p" not in lines
    assert "StrictHostKeyChecking=no" in out


def test_stream_ssh_prefix_uses_sshpass():
    out = _run("avsync_heartbeat_ssh_prefix_argv stream")
    lines = [l for l in out.splitlines() if l != ""]
    assert lines[0] == "sshpass"
    assert "-p" in lines
    assert any("@10.77.9.204" in l for l in lines)


def test_stream_ssh_prefix_honors_env_overrides():
    out = _run(
        "avsync_heartbeat_ssh_prefix_argv stream",
        {"STREAM_PW": "secretpw", "STREAM_USER": "svc", "STREAM_IP": "10.0.0.9"},
    )
    lines = [l for l in out.splitlines() if l != ""]
    assert "secretpw" in lines
    assert "svc@10.0.0.9" in lines


def test_default_ssh_prefix_matches_dev2():
    assert _run("avsync_heartbeat_ssh_prefix_argv") == _run("avsync_heartbeat_ssh_prefix_argv dev2")


# ── vlc-leg presence (skip on dev2, present on stream) ───────────────────────


def test_has_vlc_leg_false_on_dev2_true_on_stream():
    assert _run("avsync_heartbeat_has_vlc_leg dev2 && echo YES || echo NO").strip() == "NO"
    assert _run("avsync_heartbeat_has_vlc_leg stream && echo YES || echo NO").strip() == "YES"


def test_has_vlc_leg_default_is_dev2_no():
    assert _run("avsync_heartbeat_has_vlc_leg && echo YES || echo NO").strip() == "NO"


# ── the dev2 probe output still parses through the SHARED extractor/parsers ───


def test_dev2_probe_output_parses_watchdog_segment_and_epoch():
    # Simulate dev2's `cat` output: one heartbeat line, then the separator, then an EMPTY vlc half.
    body = (
        'OUT="$(printf \'%s\\n%s\\n\' "1700000000\tmeasured: db=-5.4 [x] AV offset +0 fr (+0 ms) conf 8.0 :: A/V sync OK (offset 0 ms)" "'
        + _SEP
        + '")"\n'
        'seg="$(avsync_heartbeat_extract_segment "$OUT" watchdog)"\n'
        'echo "EPOCH=$(avsync_heartbeat_last_epoch "$seg")"\n'
        'echo "VLC=[$(avsync_heartbeat_extract_segment "$OUT" vlc)]"\n'
    )
    out = _run(body)
    assert "EPOCH=1700000000" in out
    assert "VLC=[]" in out   # dev2 has no vlc heartbeat -> empty, never a spurious value
