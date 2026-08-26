"""#1199 -- tests for the strih on-box NIC-fail self-heal watcher.

TWO layers, matching the repo's "pure core + static-anchor mirror" pattern (same as
tests/python/test_avsync_lineup.py <-> avsync-watchdog.ps1):

  1. BEHAVIOURAL (RED->GREEN): unit tests over the PURE decision core
     scripts/strih_nic_selfheal_decision.py -- the single source of truth for the ladder
     state machine. Runs on dev1 CI (plain python, no Windows, no network).

  2. STATIC: validate scripts/strih-nic-selfheal.ps1 + scripts/install-strih-nic-selfheal.ps1
     structurally (there is no pwsh runtime on dev1 CI). Anchors pin the load-bearing invariants:
     ladder thresholds present AND equal to the python constants (the mirror stays in lock-step),
     NO Restart-NetAdapter rung (owner ruling 2026-08-25 -- disable/enable HANGS on strih),
     `shutdown /r` (a reboot) and NEVER `shutdown /s` (a power-off), the fail-safe UNKNOWN branch,
     the reboot-cap transition logic (#1199 review W3), the reboot-suppressed-on-persist-failure
     guard (review W2), the best-effort OBS-WS graceful-stop guard, the state-file schema, and the
     SYSTEM-every-2-min scheduled task.
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

# --- classify_pass: fail-safe direction (reachable, clean, threw) --------------------------------

def test_classify_any_reachable_is_alive():
    assert d.classify_pass(1, 3, 0) == "alive"
    assert d.classify_pass(3, 3, 0) == "alive"


def test_classify_reachable_wins_even_with_a_throw():
    # a target answered -> the NIC works -> alive, regardless of another target throwing.
    assert d.classify_pass(1, 2, 1) == "alive"


def test_classify_all_clean_negatives_is_dead():
    assert d.classify_pass(0, 3, 0) == "dead"
    assert d.classify_pass(0, 1, 0) == "dead"


def test_classify_partial_throw_no_reachable_is_unknown_not_dead():
    # #1199 review W1: a pass is 'dead' ONLY when EVERY probe returned a clean negative. One throw
    # with nothing reachable can never PROVE a total outage -> unknown, never dead.
    assert d.classify_pass(0, 2, 1) == "unknown"


def test_classify_all_threw_is_unknown():
    assert d.classify_pass(0, 0, 3) == "unknown"


def test_classify_nothing_probed_is_unknown():
    assert d.classify_pass(0, 0, 0) == "unknown"


def test_classify_non_numeric_is_unknown():
    assert d.classify_pass(None, 3, 0) == "unknown"
    assert d.classify_pass("x", "y", "z") == "unknown"


# --- constants sanity ----------------------------------------------------------------------------

def test_ladder_constants():
    assert d.DEAD_PASSES_BEFORE_REBOOT == 5
    assert d.MAX_REBOOTS == 2
    assert d.PROBE_INTERVAL_MIN == 2
    # owner ruling: there is no adapter-restart rung, so no restart threshold exists.
    assert not hasattr(d, "DEAD_PASSES_BEFORE_RESTART")


def test_no_restart_adapter_action_exists():
    # the ladder's only self-heal action is a reboot -- never an adapter restart.
    assert "restart_adapter" not in d.ACTIONS
    assert set(d.ACTIONS) == {"none", "reboot", "give_up"}


# --- unknown never advances nor resets -----------------------------------------------------------

def test_unknown_pass_leaves_state_untouched_midstreak():
    st = {"phase": "armed", "consecutive_dead": 4, "reboots": 0}
    action, ns, _ = d.decide(st, "unknown")
    assert action == "none"
    assert ns == {"phase": "armed", "consecutive_dead": 4, "reboots": 0}


def test_unknown_pass_does_not_reset_after_a_reboot():
    st = {"phase": "armed", "consecutive_dead": 3, "reboots": 1}
    action, ns, _ = d.decide(st, "unknown")
    assert action == "none"
    assert ns == {"phase": "armed", "consecutive_dead": 3, "reboots": 1}


# --- alive resets everything ---------------------------------------------------------------------

def test_alive_resets_all_counters_and_phase():
    st = {"phase": "exhausted", "consecutive_dead": 4, "reboots": 2}
    action, ns, _ = d.decide(st, "alive")
    assert action == "none"
    assert ns == {"phase": "armed", "consecutive_dead": 0, "reboots": 0}


# --- the ladder: armed -> reboot (single step, no restart rung) ----------------------------------

def test_armed_below_threshold_just_counts():
    st = d.initial_state()
    for i in range(1, d.DEAD_PASSES_BEFORE_REBOOT):
        action, st, _ = d.decide(st, "dead")
        assert action == "none", "pass %d should not act yet" % i
        assert st["phase"] == "armed"
        assert st["consecutive_dead"] == i
        assert st["reboots"] == 0


def test_fifth_dead_fires_reboot_directly_no_restart_first():
    st = d.initial_state()
    action = "none"
    for _ in range(d.DEAD_PASSES_BEFORE_REBOOT):
        action, st, _ = d.decide(st, "dead")
    assert action == "reboot"       # NOT restart_adapter -- there is no such rung
    assert st["phase"] == "armed"
    assert st["consecutive_dead"] == 0
    assert st["reboots"] == 1


def test_second_reboot_after_another_dead_window():
    st = {"phase": "armed", "consecutive_dead": 0, "reboots": 1}
    action = "none"
    for _ in range(d.DEAD_PASSES_BEFORE_REBOOT):
        action, st, _ = d.decide(st, "dead")
    assert action == "reboot"
    assert st["reboots"] == 2
    assert st["phase"] == "armed"


def test_reboot_cap_gives_up_after_max_reboots():
    st = {"phase": "armed", "consecutive_dead": 0, "reboots": d.MAX_REBOOTS}
    action = "none"
    for _ in range(d.DEAD_PASSES_BEFORE_REBOOT):
        action, st, _ = d.decide(st, "dead")
    assert action == "give_up"
    assert st["phase"] == "exhausted"
    assert st["reboots"] == d.MAX_REBOOTS  # never exceeds the cap


def test_exhausted_never_reboots_again():
    st = {"phase": "exhausted", "consecutive_dead": 0, "reboots": d.MAX_REBOOTS}
    for _ in range(3 * d.DEAD_PASSES_BEFORE_REBOOT):
        action, st, _ = d.decide(st, "dead")
        assert action == "none"
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
    assert st["phase"] == "exhausted"


def test_decide_never_mutates_input_state():
    st = {"phase": "armed", "consecutive_dead": 4, "reboots": 0}
    d.decide(st, "dead")
    assert st == {"phase": "armed", "consecutive_dead": 4, "reboots": 0}


def test_corrupt_state_is_normalized_to_armed_not_crashed():
    action, ns, _ = d.decide({"phase": "garbage", "consecutive_dead": "x", "reboots": None}, "dead")
    assert action == "none"
    assert ns["phase"] == "armed"
    assert ns["consecutive_dead"] == 1


# =================================================================================================
# Layer 2 -- STATIC validation of the watcher ps1
# =================================================================================================

def test_watcher_exists():
    assert WATCHER.exists(), "%s must exist" % WATCHER


def test_watcher_probes_and_reads_nic():
    s = _watcher()
    assert "Test-Connection" in s
    assert "Get-NetAdapter" in s  # read-only, for the log


def test_watcher_has_no_adapter_restart_rung_owner_ruling():
    # owner ruling 2026-08-25: NIC disable/enable HANGS on strih -- never any adapter fiddling.
    s = _watcher()
    assert "Restart-NetAdapter" not in s
    assert "Disable-NetAdapter" not in s
    assert "Enable-NetAdapter" not in s


def test_watcher_ladder_constants_match_python_mirror():
    s = _watcher()
    m_reboot = re.search(r"\$DeadPassesBeforeReboot\s*=\s*(\d+)", s)
    m_max = re.search(r"\$MaxReboots\s*=\s*(\d+)", s)
    assert m_reboot and int(m_reboot.group(1)) == d.DEAD_PASSES_BEFORE_REBOOT
    assert m_max and int(m_max.group(1)) == d.MAX_REBOOTS
    # the removed restart-threshold must not linger in the ps1 either.
    assert "DeadPassesBeforeRestart" not in s


def test_watcher_reboots_never_powers_off():
    s = _watcher()
    assert re.search(r"shutdown\s+/r\b", s), "must issue a graceful REBOOT (shutdown /r)"
    assert not re.search(r"shutdown\s+/s\b", s), "must NEVER power the box OFF (shutdown /s)"


def test_watcher_failsafe_unknown_branch():
    s = _watcher()
    assert "'unknown'" in s, "must classify a probe error / nothing-probed as 'unknown'"
    assert re.search(r"fail[- ]safe|fail toward inaction", s, re.IGNORECASE)


def test_watcher_reboot_cap_transition_logic_present():
    # #1199 review W3: pin the safety-critical transitions, not just the constants (no pwsh on CI).
    s = _watcher()
    assert re.search(r"\$rb\s*-lt\s*\$MaxReboots", s), "the reboot-cap guard must be present"
    assert re.search(r"Reboots\s*=\s*\(\$rb\s*\+\s*1\)", s), "a reboot must increment reboots"
    assert "give_up" in s and "'exhausted'" in s, "the cap -> give_up/exhausted branch must be present"


def test_watcher_refuses_reboot_when_state_not_persisted():
    # #1199 review W2: a reboot must be gated on the incremented state being durably written,
    # else after the reboot the box re-reads stale state and reboots past the cap (unbounded loop).
    s = _watcher()
    assert re.search(r"REBOOT SUPPRESSED", s)
    assert re.search(r"if\s*\(\s*\$persisted\s*\)", s)


def test_watcher_ws_stop_is_best_effort_never_a_hard_dependency():
    s = _watcher()
    assert "Invoke-ObsGracefulStop" in s
    assert "4455" in s
    assert "ClientWebSocket" in s
    assert "-WsPassword" in s or "$WsPassword" in s
    assert "STRIH_OBS_WS_PASSWORD" in s
    assert "obs-ws-password.txt" in s
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
    assert re.search(r"\$IntervalMinutes\s*=\s*2", s)
    assert "RepetitionInterval" in s
    assert "New-TimeSpan -Minutes" in s
    assert "powershell.exe" in s


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
