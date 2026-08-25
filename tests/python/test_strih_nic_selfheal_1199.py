"""#1199 -- tests for the strih on-box NIC-fail self-heal watcher.

TWO layers, matching the repo's "pure core + static-anchor mirror" pattern (same as
tests/python/test_avsync_lineup.py <-> avsync-watchdog.ps1):

  1. BEHAVIOURAL (RED->GREEN): unit tests over the PURE decision core
     scripts/strih_nic_selfheal_decision.py -- the single source of truth for the ladder
     state machine. Runs on dev1 CI (plain python, no Windows, no network).

  2. STATIC: validate scripts/strih-nic-selfheal.ps1 + scripts/install-strih-nic-selfheal.ps1
     structurally (there is no pwsh runtime on dev1 CI). Anchors pin the load-bearing invariants:
     ladder thresholds present AND equal to the python constants (the mirror stays in lock-step),
     `shutdown /r` (a reboot) and NEVER `shutdown /s` (a power-off), the fail-safe UNKNOWN branch,
     the best-effort OBS-WS graceful-stop guard, the state-file schema, and the SYSTEM-every-2-min
     scheduled task.
"""

import pathlib
import re
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import strih_nic_selfheal_decision as d  # noqa: E402

WATCHER = _SCRIPTS / "strih-nic-selfheal.ps1"
INSTALLER = _SCRIPTS / "install-strih-nic-selfheal.ps1"
REACH_WD = _SCRIPTS / "network-reach-alert-watchdog.sh"


def _watcher():
    return WATCHER.read_text(encoding="utf-8")


def _installer():
    return INSTALLER.read_text(encoding="utf-8")


# =================================================================================================
# Layer 1 -- PURE decision core (behavioural, RED->GREEN)
# =================================================================================================

# --- classify_pass: fail-safe direction ----------------------------------------------------------

def test_classify_any_reachable_is_alive():
    assert d.classify_pass(1, 3, False) == "alive"
    assert d.classify_pass(3, 3, False) == "alive"


def test_classify_all_clean_negatives_is_dead():
    assert d.classify_pass(0, 3, False) == "dead"
    assert d.classify_pass(0, 1, False) == "dead"


def test_classify_probe_error_is_unknown_never_dead():
    # the WHOLE fail-safe point: a probe that could not run is UNKNOWN, never a dead vote.
    assert d.classify_pass(0, 0, True) == "unknown"
    assert d.classify_pass(0, 3, True) == "unknown"


def test_classify_nothing_probed_is_unknown():
    assert d.classify_pass(0, 0, False) == "unknown"


def test_classify_non_numeric_is_unknown():
    assert d.classify_pass(None, 3, False) == "unknown"
    assert d.classify_pass("x", "y", False) == "unknown"


# --- constants sanity ----------------------------------------------------------------------------

def test_ladder_constants():
    assert d.DEAD_PASSES_BEFORE_RESTART == 5
    assert d.DEAD_PASSES_BEFORE_REBOOT == 5
    assert d.MAX_REBOOTS == 2
    assert d.PROBE_INTERVAL_MIN == 2


# --- unknown never advances nor resets -----------------------------------------------------------

def test_unknown_pass_leaves_state_untouched_midstreak():
    st = {"phase": "normal", "consecutive_dead": 4, "reboots": 0}
    action, ns, _ = d.decide(st, "unknown")
    assert action == "none"
    assert ns == {"phase": "normal", "consecutive_dead": 4, "reboots": 0}


def test_unknown_pass_does_not_reset_a_restarted_phase():
    st = {"phase": "restarted", "consecutive_dead": 3, "reboots": 1}
    action, ns, _ = d.decide(st, "unknown")
    assert action == "none"
    assert ns == {"phase": "restarted", "consecutive_dead": 3, "reboots": 1}


# --- alive resets everything ---------------------------------------------------------------------

def test_alive_resets_all_counters_and_phase():
    st = {"phase": "rebooted", "consecutive_dead": 4, "reboots": 2}
    action, ns, _ = d.decide(st, "alive")
    assert action == "none"
    assert ns == {"phase": "normal", "consecutive_dead": 0, "reboots": 0}


# --- the ladder: normal -> restart -> reboot -----------------------------------------------------

def test_normal_below_threshold_just_counts():
    st = d.initial_state()
    for i in range(1, d.DEAD_PASSES_BEFORE_RESTART):
        action, st, _ = d.decide(st, "dead")
        assert action == "none", "pass %d should not act yet" % i
        assert st["phase"] == "normal"
        assert st["consecutive_dead"] == i


def test_normal_fifth_dead_fires_restart_adapter():
    st = d.initial_state()
    action = "none"
    for _ in range(d.DEAD_PASSES_BEFORE_RESTART):
        action, st, _ = d.decide(st, "dead")
    assert action == "restart_adapter"
    assert st["phase"] == "restarted"
    assert st["consecutive_dead"] == 0
    assert st["reboots"] == 0


def test_restarted_fifth_dead_fires_reboot():
    st = {"phase": "restarted", "consecutive_dead": 0, "reboots": 0}
    action = "none"
    for _ in range(d.DEAD_PASSES_BEFORE_REBOOT):
        action, st, _ = d.decide(st, "dead")
    assert action == "reboot"
    assert st["phase"] == "rebooted"
    assert st["consecutive_dead"] == 0
    assert st["reboots"] == 1


def test_rebooted_still_dead_rearms_with_adapter_restart():
    st = {"phase": "rebooted", "consecutive_dead": 0, "reboots": 1}
    action = "none"
    for _ in range(d.DEAD_PASSES_BEFORE_RESTART):
        action, st, _ = d.decide(st, "dead")
    assert action == "restart_adapter"
    assert st["phase"] == "restarted"
    assert st["reboots"] == 1  # a re-arm does not consume another reboot


def test_reboot_cap_gives_up_after_max_reboots():
    # already used up the reboot budget; the next reboot decision point must GIVE UP, not reboot.
    st = {"phase": "restarted", "consecutive_dead": 0, "reboots": d.MAX_REBOOTS}
    action = "none"
    for _ in range(d.DEAD_PASSES_BEFORE_REBOOT):
        action, st, _ = d.decide(st, "dead")
    assert action == "give_up"
    assert st["phase"] == "exhausted"
    assert st["reboots"] == d.MAX_REBOOTS  # never exceeds the cap


def test_exhausted_only_does_cheap_adapter_restarts_never_reboots():
    st = {"phase": "exhausted", "consecutive_dead": 0, "reboots": d.MAX_REBOOTS}
    action = "none"
    for _ in range(d.DEAD_PASSES_BEFORE_RESTART):
        action, st, _ = d.decide(st, "dead")
    assert action == "restart_adapter"
    assert st["phase"] == "exhausted"
    assert st["reboots"] == d.MAX_REBOOTS


def test_full_outage_walk_fires_exactly_two_reboots_then_gives_up():
    """End-to-end: a NIC that never recovers must reboot at most MAX_REBOOTS times, then stop."""
    st = d.initial_state()
    reboots = 0
    give_ups = 0
    for _ in range(400):  # ~13h of 2-min passes, all dead
        action, st, _ = d.decide(st, "dead")
        if action == "reboot":
            reboots += 1
        elif action == "give_up":
            give_ups += 1
    assert reboots == d.MAX_REBOOTS
    assert give_ups >= 1
    assert st["reboots"] == d.MAX_REBOOTS


def test_decide_never_mutates_input_state():
    st = {"phase": "normal", "consecutive_dead": 4, "reboots": 0}
    d.decide(st, "dead")
    assert st == {"phase": "normal", "consecutive_dead": 4, "reboots": 0}


def test_corrupt_state_is_normalized_not_crashed():
    action, ns, _ = d.decide({"phase": "garbage", "consecutive_dead": "x", "reboots": None}, "dead")
    assert action == "none"
    assert ns["phase"] == "normal"
    assert ns["consecutive_dead"] == 1


# =================================================================================================
# Layer 2 -- STATIC validation of the watcher ps1
# =================================================================================================

def test_watcher_exists():
    assert WATCHER.exists(), "%s must exist" % WATCHER


def test_watcher_probes_and_reads_nic():
    s = _watcher()
    assert "Test-Connection" in s
    assert "Get-NetAdapter" in s
    assert "Restart-NetAdapter" in s


def test_watcher_ladder_constants_match_python_mirror():
    s = _watcher()
    m_restart = re.search(r"\$DeadPassesBeforeRestart\s*=\s*(\d+)", s)
    m_reboot = re.search(r"\$DeadPassesBeforeReboot\s*=\s*(\d+)", s)
    m_max = re.search(r"\$MaxReboots\s*=\s*(\d+)", s)
    assert m_restart and int(m_restart.group(1)) == d.DEAD_PASSES_BEFORE_RESTART
    assert m_reboot and int(m_reboot.group(1)) == d.DEAD_PASSES_BEFORE_REBOOT
    assert m_max and int(m_max.group(1)) == d.MAX_REBOOTS


def test_watcher_reboots_never_powers_off():
    s = _watcher()
    assert re.search(r"shutdown\s+/r\b", s), "must issue a graceful REBOOT (shutdown /r)"
    assert not re.search(r"shutdown\s+/s\b", s), "must NEVER power the box OFF (shutdown /s)"


def test_watcher_failsafe_unknown_branch():
    s = _watcher()
    assert "'unknown'" in s, "must classify a probe error / nothing-probed as 'unknown'"
    assert re.search(r"fail[- ]safe|fail toward inaction", s, re.IGNORECASE)


def test_watcher_ws_stop_is_best_effort_never_a_hard_dependency():
    s = _watcher()
    assert "Invoke-ObsGracefulStop" in s
    assert "4455" in s
    assert "ClientWebSocket" in s
    # a -WsPassword param + env + secret-file fallback
    assert "-WsPassword" in s or "$WsPassword" in s
    assert "STRIH_OBS_WS_PASSWORD" in s
    assert "obs-ws-password.txt" in s
    # the WS attempt is inside try/catch and the reboot proceeds regardless
    assert re.search(r"\btry\b", s) and re.search(r"\bcatch\b", s)
    assert re.search(r"best[- ]effort", s, re.IGNORECASE)
    assert re.search(r"rebooting anyway|reboot proceeds|proceed(s)? to reboot|regardless", s, re.IGNORECASE)


def test_watcher_state_file_schema():
    s = _watcher()
    assert r"C:\ProgramData\camera-box\nic-selfheal-state.json" in s
    for key in ("phase", "consecutive_dead", "reboots"):
        assert key in s, "state schema must carry %r" % key
    assert "nic-selfheal.log" in s


def test_watcher_has_dryrun_switch():
    assert re.search(r"\[switch\]\$DryRun", _watcher())


# =================================================================================================
# Layer 2 -- STATIC validation of the installer ps1
# =================================================================================================

def test_installer_exists():
    assert INSTALLER.exists(), "%s must exist" % INSTALLER


def test_installer_deploys_into_programdata_camera_box():
    s = _installer()
    assert r"C:\ProgramData\camera-box" in s
    assert "strih-nic-selfheal.ps1" in s  # it deploys the watcher


def test_installer_registers_system_task_every_2_min():
    s = _installer()
    assert "Register-ScheduledTask" in s
    assert "New-ScheduledTaskPrincipal" in s
    assert "SYSTEM" in s
    assert re.search(r"RunLevel\s+Highest", s)
    # the 2-minute cadence: default IntervalMinutes = 2 + a repetition interval trigger
    assert re.search(r"\$IntervalMinutes\s*=\s*2", s)
    assert "RepetitionInterval" in s
    assert "New-TimeSpan -Minutes" in s
    assert "powershell.exe" in s  # 5.1 semantics to match the watcher's Test-Connection


def test_installer_has_uninstall_switch():
    s = _installer()
    assert re.search(r"\[switch\]\$Uninstall", s)
    assert "Unregister-ScheduledTask" in s


def test_installer_task_name_matches_watcher():
    assert "strih-nic-selfheal" in _installer()
    assert "strih-nic-selfheal" in _watcher()


# =================================================================================================
# Layer 2 -- the dev1 reach watchdog carries a pointer to the self-heal watcher (issue 1199 step 3)
# =================================================================================================

def test_reach_watchdog_notes_strih_self_heals():
    if not REACH_WD.exists():
        return  # optional pointer; never fail the suite if the sibling is renamed
    s = REACH_WD.read_text(encoding="utf-8")
    assert re.search(r"1199|self-heal|selfheal", s, re.IGNORECASE), \
        "network-reach-alert-watchdog.sh header should point at the strih NIC self-heal watcher"
