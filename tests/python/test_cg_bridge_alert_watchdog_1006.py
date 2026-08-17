"""#1006 — the dev1-side CG-bridge republish-black alert watchdog (scripts/cg-bridge-alert-watchdog.sh
+ scripts/lib/cg-bridge-health.sh). Tier-0 tests: they shell out to bash (no cargo, no rig, no OBS)
to pin the PURE classifier, the script's structural contract, and the ships-DISABLED convention.

The differential DECISION itself lives in `obs_phase2.py republish-black-check` (its exit code
encodes OK/IDLE/FAULT/UNKNOWN — covered by test_obs_phase2_republish_black_1006.py); this lib only
maps that probe's rc to the watchdog's incident classification, so the confirm/throttle/page flow
stays identical to every sibling dev1-side alert watchdog (scripts/lib/obs-watchdog-decision.sh).
"""
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "cg-bridge-health.sh"
_WATCHDOG = _ROOT / "scripts" / "cg-bridge-alert-watchdog.sh"
_SVC = _ROOT / "systemd" / "cg-bridge-alert-watchdog.service"
_TIMER = _ROOT / "systemd" / "cg-bridge-alert-watchdog.timer"


def _classify(rc):
    r = subprocess.run(
        ["bash", "-c", f'. "{_LIB}"; cg_bridge_classify_probe {rc}'],
        capture_output=True, text=True,
    )
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_classify_fault_from_probe_exit_3():
    # obs_phase2 republish-black-check exits 3 on FAULT (upstream live, republish black).
    assert _classify(3) == "alert:republish-black"


def test_classify_healthy_from_probe_exit_0():
    # exit 0 == OK (both live) or IDLE (upstream itself idle) — never an alarm.
    assert _classify(0) == "healthy"


def test_classify_unknown_from_probe_exit_4_or_transport_failure():
    # exit 4 == unreadable screenshot; 124 == timeout; 1 == other. All -> nothing to decide.
    assert _classify(4) == "unknown"
    assert _classify(124) == "unknown"
    assert _classify(1) == "unknown"


def test_lib_is_source_only_running_it_defines_but_does_nothing():
    # Executing the lib must produce NO output (it only DEFINES the function).
    r = subprocess.run(["bash", str(_LIB)], capture_output=True, text=True)
    assert r.returncode == 0
    assert r.stdout == "" and r.stderr == ""


def test_watchdog_syntax_is_valid():
    r = subprocess.run(["bash", "-n", str(_WATCHDOG)], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


def test_watchdog_help_exits_0_and_explains_the_differential():
    r = subprocess.run(["bash", str(_WATCHDOG), "--help"], capture_output=True, text=True)
    assert r.returncode == 0
    out = r.stdout.lower()
    assert "republish" in out and "differential" in out and "spout" in out


def test_watchdog_rejects_an_unknown_arg():
    r = subprocess.run(["bash", str(_WATCHDOG), "--nope"], capture_output=True, text=True)
    assert r.returncode == 2


def test_watchdog_reuses_the_shared_obs_watchdog_decision_lib_not_a_second_mechanism():
    body = _WATCHDOG.read_text()
    assert "obs-watchdog-decision.sh" in body
    assert "obs_watchdog_confirm" in body and "obs_watchdog_alert_throttle" in body
    # The alert path fires from dev1 via airuleset notify (same topology as the siblings).
    assert "notify" in body and "airuleset" in body.lower()


def test_watchdog_is_set_uo_pipefail_not_e():
    # A watchdog must survive a per-pass failure and keep polling (never abort on set -e).
    body = _WATCHDOG.read_text()
    assert "set -uo pipefail" in body
    assert "set -euo pipefail" not in body


def test_systemd_units_exist_and_ship_disabled():
    assert _SVC.exists() and _TIMER.exists()
    # "Ships disabled": NO installer/CI script ENABLES this timer anywhere in the repo (a doc
    # comment naming it to say it is disabled is fine — only an actual `systemctl enable`/`--now`
    # of the timer would break the convention).
    hits = []
    for sub in ("scripts", ".github"):
        base = _ROOT / sub
        if not base.exists():
            continue
        for p in base.rglob("*"):
            if not p.is_file():
                continue
            for line in _safe_read(p).splitlines():
                stripped = line.strip()
                if stripped.startswith("#"):
                    continue  # a doc comment naming the timer (to say it is disabled) is fine
                if "cg-bridge-alert-watchdog" in line and "systemctl" in line and (
                    "enable" in line or "--now" in line
                ):
                    hits.append(f"{p.relative_to(_ROOT)}: {stripped}")
    assert hits == [], f"timer must not be auto-enabled by any installer/CI: {hits}"


def _safe_read(p):
    try:
        return p.read_text()
    except (UnicodeDecodeError, OSError):
        return ""
