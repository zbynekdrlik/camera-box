"""issue 1412: provisioning gaps found live on strih-pp / stream-pp (11.10.2026).

Each test drives the REAL lib functions (sourced under the callers' `set -euo pipefail`), values passed
as positional arguments, never spliced into the script text. Tier-0: pytest + bash, no root, no rig.
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "strih-provision.sh"


def _bash(script, *args, env=None, cwd=None):
    full = 'set -euo pipefail\n. "$LIB"\n' + script
    e = dict(os.environ, LIB=str(LIB))
    if env:
        e.update(env)
    return subprocess.run(["bash", "-c", full, "harness", *map(str, args)], capture_output=True, text=True,
                          env=e, cwd=cwd, timeout=120)


def _sections(text):
    """systemd-style sections -> {section: [key=value lines]} (comments and blanks dropped)."""
    out, cur = {}, None
    for line in text.splitlines():
        s = line.strip()
        if not s or s.startswith("#"):
            continue
        if s.startswith("[") and s.endswith("]"):
            cur = s[1:-1]
            out.setdefault(cur, [])
            continue
        out.setdefault(cur, []).append(s)
    return out


# --- bug 4: the local-audio drop-in's After= must sit in [Unit] (systemd ignores it in [Service]) ---

def test_the_local_audio_dropin_orders_after_the_user_manager_in_its_unit_section():
    r = _bash('strih_intercom_audio_dropin "$1" "$2"', "newlevel", 1000)
    assert r.returncode == 0, r.stderr
    sec = _sections(r.stdout)
    assert "After=user@1000.service" in sec.get("Unit", []), sec
    assert not any(k.startswith("After=") for k in sec.get("Service", [])), sec
    # the Service half is unchanged
    for want in ("DynamicUser=no", "User=newlevel", "Environment=XDG_RUNTIME_DIR=/run/user/1000"):
        assert want in sec["Service"], (want, sec)


def test_no_unit_only_key_sits_in_the_service_section_of_the_dropin():
    r = _bash('strih_intercom_audio_dropin "$1" "$2"', "newlevel", 1000)
    unit_only = ("After=", "Before=", "Wants=", "Requires=", "BindsTo=", "PartOf=", "Description=")
    bad = [k for k in _sections(r.stdout).get("Service", []) if k.startswith(unit_only)]
    assert bad == [], bad
