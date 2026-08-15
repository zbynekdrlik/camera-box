"""#1060 — pure-decision unit tests for scripts/lib/obs-burn-reconcile-decision.sh.

The dev1-side fresh-OBS-start burn-reconcile watchdog's whole "should I sweep?" decision lives in
this ONE pure shell lib (no I/O, no OBS, no ssh — mirrors scripts/lib/obs-watchdog-decision.sh),
so it can be exhaustively unit-tested offline. These tests source the lib in a subprocess and
assert the truth table + the fresh-start detector directly (runnable locally under Tier-0 — this
is Python/bash, not `cargo test`).

The load-bearing discriminator (see the ticket's own design comment): a persistent TEST-mode burn
on strih/stream is a LEGITIMATE, deliberately-persistent operator state whose rig-active heartbeat
goes stale after ~10 min while the burn should remain. So "burn present + stale heartbeat" is NOT
a leak. Only at a FRESH OBS start is a reloaded saved burn definitively a resurrection — and even
then we DEFER while a live gate/TEST harness is coordinating (fresh heartbeat / held rig lease),
so the watchdog never clears a burn a live gate deliberately set mid-run.
"""
import pathlib
import subprocess

_LIB = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "lib" / "obs-burn-reconcile-decision.sh"


def _decide(fresh, coordinated, burn_present):
    """Source the lib and echo obs_burn_reconcile_decide's verdict."""
    script = (
        'set -uo pipefail\n'
        f'. "{_LIB}"\n'
        f'obs_burn_reconcile_decide {fresh} {coordinated} {burn_present}\n'
    )
    out = subprocess.run(["bash", "-c", script], capture_output=True, text=True)
    assert out.returncode == 0, f"decide exited nonzero: {out.stderr}"
    return out.stdout.strip()


def _is_fresh_start(prev, cur):
    """Source the lib and return obs_burn_reconcile_is_fresh_start's exit code (0=fresh, 1=not)."""
    script = (
        'set -uo pipefail\n'
        f'. "{_LIB}"\n'
        f'obs_burn_reconcile_is_fresh_start "{prev}" "{cur}"\n'
    )
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True).returncode


# ---- obs_burn_reconcile_decide: the full truth table ------------------------------------------

def test_not_a_fresh_start_is_always_noop():
    # The KEY guard: a persistent TEST-mode burn (no restart) is never touched, regardless of
    # coordination or whether a burn is present.
    for coord in (0, 1):
        for burn in (0, 1):
            assert _decide(0, coord, burn) == "NOOP", f"coord={coord} burn={burn}"


def test_fresh_start_but_coordinated_defers():
    # A live gate/TEST harness relaunched OBS and owns burn state right now — never fight it.
    assert _decide(1, 1, 0) == "DEFER"
    assert _decide(1, 1, 1) == "DEFER"


def test_fresh_start_uncoordinated_with_burn_sweeps():
    # The exact leak this ticket fixes: an unattended OBS restart resurrected a saved burn.
    assert _decide(1, 0, 1) == "SWEEP"


def test_fresh_start_uncoordinated_no_burn_is_clean():
    # A fresh start with nothing to clear — log-only, never a sweep or an alert.
    assert _decide(1, 0, 0) == "CLEAN"


# ---- obs_burn_reconcile_is_fresh_start: renderTotalFrames restart detection -------------------

def test_fresh_start_when_baseline_unknown():
    # First pass (or a lost state file): no prior baseline => reconcile once (safe — SWEEP only
    # fires when uncoordinated AND a burn renders).
    assert _is_fresh_start("", "5000") == 0
    assert _is_fresh_start("notanumber", "5000") == 0


def test_fresh_start_when_counter_dropped():
    # renderTotalFrames RESET (cur < prev) => OBS restarted since the last pass.
    assert _is_fresh_start("500000", "1200") == 0


def test_not_fresh_when_counter_climbed_or_equal():
    # Monotone increase (or steady) => same OBS session, NOT a restart.
    assert _is_fresh_start("1200", "500000") == 1
    assert _is_fresh_start("500000", "500000") == 1


def test_not_fresh_when_current_unreadable():
    # A bad/empty current read cannot prove a restart — conservatively NOT fresh (the watchdog
    # treats an unreadable probe as "nothing to decide this pass" separately).
    assert _is_fresh_start("500000", "") == 1
    assert _is_fresh_start("500000", "notanumber") == 1
