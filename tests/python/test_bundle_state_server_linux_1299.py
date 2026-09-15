"""#1299 Part 2 — the canonical bundle-state-server must DEGRADE cleanly on Linux (imag) so the
fleet-visible genlock-lock facet actually covers imag.

Two proofs:

1. gather_bundle_state on a simulated Linux platform (IS_WINDOWS=False) must SKIP every Windows-only
   identity gather (native tasklist/netstat/CIM process reads, ProgramData DLL hashes, the .lnk/AHK
   Start-Menu paths, the PowerShell NDI-runtime read) — at the gather boundary, so no Windows-only
   subprocess is spawned and no WARNING is logged per request — while the log-derived facets
   (genlock_lock, audio_ts_lag) and the cross-platform genlock_build_sha keep serving, and nothing
   raises. The Windows path (IS_WINDOWS=True) must stay byte-identical (the identity gathers are
   still called).

2. Static string-pins that the new Linux install path is wired: the systemd unit's ExecStart carries
   the imag flags, and setup-imag.sh's step 28 installs the three sibling files + the unit ENABLE-ONLY
   and bumps TOTAL_STEPS. Pure text reads (no box, no network) — a python sibling of the setup_imag_*
   static anchors, kept out of the Rust anchor file so it stays Tier-0-runnable via pytest.

Loaded by file path via importlib (bundle-state-server.py is hyphenated + __main__-guarded, so it
never starts the HTTP server), under a DISTINCT module name so it never collides with the other
test_bundle_state_server_*.py files in one pytest process.
"""
import importlib.util
import pathlib
import sys

_REPO = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _REPO / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

_SPEC = importlib.util.spec_from_file_location(
    "bundle_state_server_linux_1299", _SCRIPTS / "bundle-state-server.py"
)
bss = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(bss)  # __name__ != "__main__" -> main()/serve_forever() does NOT run


# The Windows-only identity gathers that MUST be skipped at the gather boundary on Linux. Each is a
# native subprocess (tasklist/netstat/PowerShell) or a Windows-path scan/hash that has no meaning on
# imag; on Linux they only fail-soft to "" while spawning a Windows-only binary + logging noise.
_WINDOWS_LEAF_HELPERS_ON_BSS = (
    "tasklist_csv",
    "port4455_owner",
    "resolve_shortcut",
    "read_ahk_text",
    "ndi_runtime_version",
    "gather_vb_matrix_facet",
)
_WINDOWS_LEAF_HELPERS_ON_BSG = (
    "distroav_dll_paths",
    "obs_installs_under",
    "component_sha256",  # obs.dll + distroav.dll byte hashes (Windows install paths)
)

_GENLOCK_LOCK_LINE = (
    "18:00:00.000: genlock-lock-json: "
    '{"v":2,"state":"LOCKED","reason":"ok","n_inputs":1,"n_locked":1,"n_absent":0}\n'
)


def _install_call_recorder(monkeypatch, called):
    """Wrap every Windows-only leaf helper so a CALL is recorded (not raised) — the gate must never
    call them on Linux, but if it does we want the offending name, not an opaque crash."""
    for name in _WINDOWS_LEAF_HELPERS_ON_BSS:
        orig = getattr(bss, name)

        def make(n, o):
            def wrapper(*a, **k):
                called.append(n)
                return o(*a, **k)
            return wrapper

        monkeypatch.setattr(bss, name, make(name, orig))
    for name in _WINDOWS_LEAF_HELPERS_ON_BSG:
        orig = getattr(bss.bsg, name)

        def makeg(n, o):
            def wrapper(*a, **k):
                called.append(n)
                return o(*a, **k)
            return wrapper

        monkeypatch.setattr(bss.bsg, name, makeg(name, orig))


def _gather_on_linux(monkeypatch, tmp_path, *, sha_text=None):
    """Run gather_bundle_state with IS_WINDOWS forced False, OBS-WS stubbed empty, a genlock-lock log,
    and (optionally) a real GENLOCK_BUILD_SHA.txt. Returns (state, called-helper-names)."""
    monkeypatch.setattr(bss, "IS_WINDOWS", False, raising=False)
    monkeypatch.setattr(bss, "gather_ndi_inputs", lambda host, password: {})
    called = []
    _install_call_recorder(monkeypatch, called)

    log_dir = tmp_path / "logs"
    log_dir.mkdir()
    (log_dir / "obs.txt").write_text(_GENLOCK_LOCK_LINE, encoding="utf-8")

    sha_file = tmp_path / "GENLOCK_BUILD_SHA.txt"
    if sha_text is not None:
        sha_file.write_text(sha_text + "\n", encoding="utf-8")

    state = bss.gather_bundle_state(
        "127.0.0.1", "", str(log_dir), str(tmp_path / "missing-ndi.dll"), [],
        genlock_build_sha_file=str(sha_file),
        startup_shortcut=str(tmp_path / "missing.lnk"),
        ahk_path=str(tmp_path / "missing.ahk"),
        obs_dll_path=str(tmp_path / "missing-obs.dll"),
    )
    return state, called


# --- 1. the gather degrade (RED -> GREEN) -----------------------------------------------------

def test_is_windows_flag_exists():
    # The boundary gate needs a module-level platform flag; on this posix host it must be False.
    assert hasattr(bss, "IS_WINDOWS"), "bundle-state-server.py must define IS_WINDOWS at module scope"
    assert bss.IS_WINDOWS is False, "IS_WINDOWS must be False on a posix host (os.name != 'nt')"


def test_linux_gather_skips_every_windows_identity_gather(monkeypatch, tmp_path):
    _state, called = _gather_on_linux(monkeypatch, tmp_path)
    offenders = sorted(set(called))
    assert offenders == [], (
        "on Linux the Windows-only identity gathers must be SKIPPED at the gather boundary, "
        f"but these were still invoked: {offenders}"
    )


def test_linux_gather_omits_the_windows_identity_facets(monkeypatch, tmp_path):
    state, _called = _gather_on_linux(monkeypatch, tmp_path)
    for k in (
        "obs_process_count",
        "port4455_owner_path", "port4455_owner_version",
        "ahk_app1_shortcut_path", "ahk_app1_run", "ahk_dead_config_present",
        "shortcut_target_path", "shortcut_workdir",
        "distroav_dll_paths", "distroav_dll_sha256", "obs_dll_sha256",
        "obs_installs", "ndi_runtime",
        "vb_matrix_running", "vb_matrix_name", "vb_matrix_pid", "vb_matrix_start",
    ):
        assert k not in state, f"Windows-only facet {k!r} must be OMITTED on Linux (never a false value)"


def test_linux_gather_keeps_serving_the_log_and_build_sha_facets(monkeypatch, tmp_path):
    state, _called = _gather_on_linux(monkeypatch, tmp_path, sha_text="deadbeefcafe1299")
    # log-derived genlock LOCK facet still flows (the whole point of the imag :8899 server)
    assert isinstance(state.get("genlock_lock"), dict)
    assert state["genlock_lock"]["state"] == "LOCKED"
    # the cross-platform build-SHA read (imag serves /opt/obs-genlock/GENLOCK_BUILD_SHA.txt) still flows
    assert state.get("genlock_build_sha") == "deadbeefcafe1299"


def test_linux_gather_never_raises_and_calls_the_cross_platform_build_sha(monkeypatch, tmp_path):
    # genlock_build_sha_from_file must STILL be called on Linux (it is lifted out of the gate); the
    # gated component_sha256 must NOT (that is a Windows-path hash).
    monkeypatch.setattr(bss, "IS_WINDOWS", False, raising=False)
    monkeypatch.setattr(bss, "gather_ndi_inputs", lambda host, password: {})
    build_sha_calls = []
    orig = bss.bsg.genlock_build_sha_from_file
    monkeypatch.setattr(
        bss.bsg, "genlock_build_sha_from_file",
        lambda p: build_sha_calls.append(p) or orig(p),
    )
    log_dir = tmp_path / "logs"
    log_dir.mkdir()
    (log_dir / "obs.txt").write_text(_GENLOCK_LOCK_LINE, encoding="utf-8")
    (tmp_path / "sha.txt").write_text("abc123\n", encoding="utf-8")
    # must not raise
    bss.gather_bundle_state(
        "127.0.0.1", "", str(log_dir), str(tmp_path / "x.dll"), [],
        genlock_build_sha_file=str(tmp_path / "sha.txt"),
        startup_shortcut=str(tmp_path / "m.lnk"),
        ahk_path=str(tmp_path / "m.ahk"),
        obs_dll_path=str(tmp_path / "o.dll"),
    )
    assert build_sha_calls == [str(tmp_path / "sha.txt")], (
        "genlock_build_sha_from_file must still be called on Linux (it is not a Windows-only gather)"
    )


def test_windows_path_still_calls_the_identity_gathers(monkeypatch, tmp_path):
    # The gate is a no-op on Windows: with IS_WINDOWS=True the identity gathers are still invoked, so
    # the Windows fleet's served JSON is byte-identical to before.
    monkeypatch.setattr(bss, "IS_WINDOWS", True, raising=False)
    monkeypatch.setattr(bss, "gather_ndi_inputs", lambda host, password: {})
    called = []
    _install_call_recorder(monkeypatch, called)
    log_dir = tmp_path / "logs"
    log_dir.mkdir()
    (log_dir / "obs.txt").write_text(_GENLOCK_LOCK_LINE, encoding="utf-8")
    bss.gather_bundle_state(
        "127.0.0.1", "", str(log_dir), str(tmp_path / "x.dll"), [],
        genlock_build_sha_file=str(tmp_path / "sha.txt"),
        startup_shortcut=str(tmp_path / "m.lnk"),
        ahk_path=str(tmp_path / "m.ahk"),
        obs_dll_path=str(tmp_path / "o.dll"),
    )
    # at least the pure Windows subprocess leaves must have been reached on the Windows path
    for expected in ("tasklist_csv", "port4455_owner", "resolve_shortcut"):
        assert expected in called, f"{expected} must still run on the Windows path (gate is Windows no-op)"


# --- 2. the imag install path is wired (static string-pins) -----------------------------------

_UNIT = _REPO / "systemd" / "imag-bundle-state-server.service"
_SETUP = (_SCRIPTS / "setup-imag.sh").read_text(encoding="utf-8")


def test_unit_file_exists_with_the_imag_execstart_flags():
    assert _UNIT.is_file(), "systemd/imag-bundle-state-server.service must exist"
    unit = _UNIT.read_text(encoding="utf-8")
    execstart = next((l for l in unit.splitlines() if l.strip().startswith("ExecStart=")), "")
    assert execstart, "the unit must declare an ExecStart="
    assert "/opt/camera-box/bundle-state-server.py" in execstart
    assert "--port 8899" in execstart
    assert "--obs-host 127.0.0.1" in execstart
    assert "--obs-log-dir" in execstart and "obs-studio/logs" in execstart
    assert "--genlock-build-sha-file /opt/obs-genlock/GENLOCK_BUILD_SHA.txt" in execstart
    # ENABLE-ONLY provisioning => on-failure restart, no aggressive mount-namespace hardening
    assert "Restart=on-failure" in unit
    assert "RestartSec=" in unit


def test_setup_imag_step_28_installs_the_server_enable_only():
    assert "TOTAL_STEPS=28" in _SETUP, "setup-imag.sh must bump TOTAL_STEPS to 28 for the new step"
    assert 'step 28 "' in _SETUP, "setup-imag.sh must add a step 28 banner"
    # the three sibling files land together in /opt/camera-box so the server's sibling imports resolve
    assert "/opt/camera-box" in _SETUP
    for f in ("bundle-state-server.py", "bundle_state_gather.py", "obs_phase2.py"):
        assert f in _SETUP, f"step 28 must install {f} to the box"
    assert "imag-bundle-state-server.service" in _SETUP
    # ENABLE-ONLY: never a live --now start from the provisioner
    assert "systemctl --user enable imag-bundle-state-server.service" in _SETUP
    assert "systemctl --user enable --now imag-bundle-state-server.service" not in _SETUP
