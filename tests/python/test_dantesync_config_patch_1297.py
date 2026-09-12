"""#1297 -- unit tests for the PURE dantesync config.json patcher
(scripts/dantesync_config_patch.py).

RESOLUME-SNV's dantesync shipped with NO system.phase_slew key, so it STEPS its UTC phase (the
issue-1130 storm). This helper turns the on-box flip into a reviewed, text-only transform:
enable system.phase_slew.enabled, optionally repoint ntp_server (default UNCHANGED until the
dantesync failover feature lands), and NEVER touch ntp_server_mode (the "never two masters" rule).
It is I/O-free so it RED->GREENs LOCALLY under Tier-0 (pytest runs freely; cargo is banned in this
repo). The on-box apply is emitted as text only -- no test here ever touches a box.
"""
import json
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import dantesync_config_patch as dcp  # noqa: E402


# The live RESOLUME-SNV config (supervisor read-back 2026-09-12): ntp_server_mode disabled, no
# system key at all.
_RESOLUME_CONFIG = json.dumps(
    {
        "http_status": {"enabled": True, "port": 8898},
        "ntp_server": "strih.lan",
        "ntp_server_mode": {"enabled": False, "listen_port": 123},
    },
    indent=2,
)


# ---------------------------------------------------------------- phase_slew enablement
def test_adds_system_phase_slew_enabled_true_when_absent():
    out = json.loads(dcp.patch_config(_RESOLUME_CONFIG))
    assert out["system"]["phase_slew"]["enabled"] is True


def test_phase_slew_flips_false_to_true_in_place():
    text = json.dumps({"system": {"phase_slew": {"enabled": False}}, "ntp_server": "strih.lan"})
    out = json.loads(dcp.patch_config(text))
    assert out["system"]["phase_slew"]["enabled"] is True


def test_is_idempotent():
    once = dcp.patch_config(_RESOLUME_CONFIG)
    twice = dcp.patch_config(once)
    assert once == twice


def test_preserves_sibling_keys_inside_system_and_phase_slew():
    text = json.dumps(
        {"system": {"affinity": "rt", "phase_slew": {"step_us": 2500, "enabled": False}}}
    )
    out = json.loads(dcp.patch_config(text))
    assert out["system"]["affinity"] == "rt"
    assert out["system"]["phase_slew"]["step_us"] == 2500
    assert out["system"]["phase_slew"]["enabled"] is True


# ---------------------------------------------------------------- key order preservation
def test_preserves_existing_key_order():
    out_text = dcp.patch_config(_RESOLUME_CONFIG)
    keys = list(json.loads(out_text).keys())
    # the three original keys keep their relative order; the new "system" is appended at the end.
    assert keys[:3] == ["http_status", "ntp_server", "ntp_server_mode"]
    assert keys[-1] == "system"


# ---------------------------------------------------------------- ntp_server repoint (parametrised)
def test_ntp_server_unchanged_by_default():
    out = json.loads(dcp.patch_config(_RESOLUME_CONFIG))
    assert out["ntp_server"] == "strih.lan"


def test_ntp_server_repointed_when_given():
    out = json.loads(dcp.patch_config(_RESOLUME_CONFIG, ntp_server="dev1.lan"))
    assert out["ntp_server"] == "dev1.lan"


def test_empty_ntp_server_is_rejected():
    with pytest.raises(dcp.ConfigPatchError):
        dcp.patch_config(_RESOLUME_CONFIG, ntp_server="   ")


# ---------------------------------------------------------------- never two masters
def test_ntp_server_mode_is_never_touched():
    out = json.loads(dcp.patch_config(_RESOLUME_CONFIG, ntp_server="dev1.lan"))
    assert out["ntp_server_mode"] == {"enabled": False, "listen_port": 123}


def test_ntp_server_mode_absent_stays_absent():
    text = json.dumps({"ntp_server": "strih.lan"})
    out = json.loads(dcp.patch_config(text))
    assert "ntp_server_mode" not in out


# ---------------------------------------------------------------- input validation
def test_rejects_invalid_json():
    with pytest.raises(dcp.ConfigPatchError):
        dcp.patch_config("{not json")


def test_rejects_non_object_top_level():
    with pytest.raises(dcp.ConfigPatchError):
        dcp.patch_config("[1, 2, 3]")


def test_rejects_non_object_system():
    with pytest.raises(dcp.ConfigPatchError):
        dcp.patch_config(json.dumps({"system": "rt"}))


def test_rejects_non_object_phase_slew():
    with pytest.raises(dcp.ConfigPatchError):
        dcp.patch_config(json.dumps({"system": {"phase_slew": True}}))


# ---------------------------------------------------------------- emit apply program
def test_emit_apply_program_has_backup_write_restart_readback():
    patched = dcp.patch_config(_RESOLUME_CONFIG)
    prog = dcp.emit_apply_program(r"C:\ProgramData\dantesync\config.json", patched)
    assert "Copy-Item" in prog and ".bak-" in prog  # backup
    assert "UTF8Encoding" in prog  # no-BOM write
    assert "Stop-Service" in prog and "Start-Service" in prog and "Restart-Service -Name" not in prog and "dantesync" in prog  # restart
    assert "8898/status" in prog  # read-back
    # the patched JSON is embedded verbatim in the single-quoted here-string
    assert '"enabled": true' in prog
    assert "@'" in prog and "'@" in prog


def test_emit_apply_program_honours_custom_path_and_port():
    patched = dcp.patch_config(_RESOLUME_CONFIG)
    prog = dcp.emit_apply_program(r"D:\dante\config.json", patched, service="DanteSync", status_port=9999)
    assert r"D:\dante\config.json" in prog
    assert "9999/status" in prog
    assert "DanteSync" in prog
