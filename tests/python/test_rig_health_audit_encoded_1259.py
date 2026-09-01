"""#1259 -- the two naive `powershell -NoProfile -Command "…"` OBS reads over ssh in
scripts/rig-health-audit.py (the issue-787 status-page sweep) must invoke PowerShell via
`-EncodedCommand` (base64 UTF-16LE), NEVER the naive `-Command "…| Sort-Object …"` form
Win32-OpenSSH's default cmd.exe shell mangles (the #1258 root cause, fleet-wide follow-up).

Python cannot source the shell helper scripts/lib/ps-encoded.sh, so rig-health-audit.py grows its
own `_ps_encoded()` (base64 UTF-16LE) + pure command-BUILDER functions. These pin the builders
directly (no ssh / no WS / no subprocess): the emitted command must be `-EncodedCommand <b64>` whose
payload decodes back to the intended PowerShell.
"""
import base64
import importlib.util
from pathlib import Path

HERE = Path(__file__).parent
SCRIPTS = HERE.parent.parent / "scripts"


def _load_module():
    spec = importlib.util.spec_from_file_location(
        "rig_health_audit", SCRIPTS / "rig-health-audit.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_mod = _load_module()


def _decode_encoded_payload(cmd: str) -> str:
    assert "-NoProfile -NonInteractive -EncodedCommand " in cmd, (
        f"#1259: expected a -NoProfile -NonInteractive -EncodedCommand invocation, got: {cmd!r}")
    assert '-Command "' not in cmd, (
        f"#1259: the naive -Command \"…\" form must be gone (cmd.exe mangles its pipes), got: {cmd!r}")
    b64 = cmd.split("-EncodedCommand ", 1)[1].split()[0]
    return base64.b64decode(b64).decode("utf-16-le")


def test_ps_encoded_round_trips_1259():
    sample = "gc foo | sort bar | select -last 1"
    enc = _mod._ps_encoded(sample)
    assert base64.b64decode(enc).decode("utf-16-le") == sample, (
        "#1259: _ps_encoded must base64-UTF16LE-encode its input so it round-trips exactly")


def test_windows_obs_log_tail_cmd_is_encoded_1259():
    cmd = _mod._windows_obs_log_tail_cmd(500)
    ps = _decode_encoded_payload(cmd)
    assert "Get-ChildItem $env:APPDATA\\obs-studio\\logs\\*.txt" in ps
    assert "| Sort-Object LastWriteTime -Descending" in ps
    assert "| Select-Object -First 1" in ps
    assert "Get-Content $l.FullName -TotalCount 600" in ps
    assert "Get-Content $l.FullName -Tail 500" in ps, (
        f"#1259: the log-tail builder must carry -Tail 500 through the encoded payload, got: {ps!r}")


def test_windows_obs_count_cmd_is_encoded_1259():
    cmd = _mod._windows_obs_count_cmd()
    ps = _decode_encoded_payload(cmd)
    assert ps == "(Get-Process obs64 -ErrorAction SilentlyContinue).Count", (
        f"#1259: the obs64-count builder must decode to the exact Get-Process expression, got: {ps!r}")


def test_log_tail_cmd_tail_is_numeric_clamped_1259():
    # A non-numeric tail must never inject into the encoded payload (the #1258 injection guard).
    cmd = _mod._windows_obs_log_tail_cmd("1; Remove-Item C:\\evil")
    ps = _decode_encoded_payload(cmd)
    assert "Remove-Item" not in ps and "-Tail 500" in ps, (
        f"#1259: a non-numeric tail must clamp to the 500 default, not inject, got: {ps!r}")
