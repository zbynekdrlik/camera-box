"""#1308 -- the ONE shared production-critical re-ping key helper (bash + python twins, one contract).

WHY (owner ruling ROZHODNUTÉ 2026-09-13, verbatim: „aj ntp aj ostatne veci bez ktorych nevie
produkcia bezat spravne musi notifikovat ... byt o tom dokolecka notifikovany"): a background fault
that production cannot run without (a lost dante clock, a wedged OBS, a dead bundle-state server, a
missing VB-Matrix ...) must be RE-pinged repeatedly while it PERSISTS, not paged once and then
silently card-edited forever. #1307 built the FIRST member of this class (dantesync-clock) with a
private time-bucketed key inside scripts/dantesync_clock_decision.py. #1308 GENERALISES that ONE
mechanism into a single shared helper -- a bash function every alert-watchdog already sources
(scripts/lib/obs-watchdog-decision.sh :: watchdog_notify_key) and a pure python twin
(scripts/watchdog_reping.py) with the IDENTICAL function -- so all 11 production-critical watchdogs
bucket their --dedup-key the SAME way and nobody hand-rolls a second implementation.

Contract (both twins): watchdog_notify_key(base, now, interval) -> "<base>-<floor(now/interval)>".
  - interval default 600 s (10 min); a non-numeric interval falls back to 600 (never a crash).
  - interval floored at 60 s (a smaller value is CLAMPED to 60, never a per-pass phone flood).
  - within one interval an identical state yields the SAME key (airuleset edits the card, no ping);
    the next interval yields a FRESH key (a new ping while the fault persists -- „dokolecka").
  - recovery is NOT this helper's concern; it stays ONE machine-channel log line per watchdog.

These are Tier-0 invariants (#557 kills local cargo): the python twin is exercised directly; the
bash twin is sourced in a subprocess and diffed against the python over a shared (base, now,
interval) vector (the repo's replica-parity pattern). A worktree-isolated lane cannot run
`bash -c 'source lib'` from the Bash tool, but a pytest invoked via `python3 -m pytest` may spawn
bash internally -- so the parity test runs here AND at CI.
"""
import importlib.util
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_LIB = _SCRIPTS / "lib" / "obs-watchdog-decision.sh"


def _load(mod_name, rel):
    spec = importlib.util.spec_from_file_location(mod_name, _SCRIPTS / rel)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _reping():
    return _load("watchdog_reping", "watchdog_reping.py")


def _dante():
    # dantesync_clock_decision imports watchdog_reping at load time -- prove the delegation import
    # works under importlib exec (scripts/ not on sys.path) too.
    return _load("dantesync_clock_decision", "dantesync_clock_decision.py")


# --------------------------------------------------------------------- python twin: notify_key
def test_same_key_within_a_bucket():
    r = _reping()
    assert r.notify_key("wd-cam1", 600000, 600) == r.notify_key("wd-cam1", 600599, 600)


def test_fresh_key_at_next_bucket():
    r = _reping()
    assert r.notify_key("wd-cam1", 600000, 600) != r.notify_key("wd-cam1", 600600, 600)


def test_interval_floor_60():
    r = _reping()
    # an interval below 60 is clamped to 60: 120 and 179 share bucket floor(t/60)=2, 180 -> 3.
    assert r.notify_key("b", 120, 10) == r.notify_key("b", 179, 10)
    assert r.notify_key("b", 120, 10) != r.notify_key("b", 180, 10)


def test_negative_interval_clamped_to_floor_not_default():
    # a negative int is still a valid int -> clamped to the 60 floor (matches int() semantics),
    # NOT the 600 non-numeric default -- pinned so the bash `${x#-}` parity trick stays honest.
    r = _reping()
    assert r.reping_interval(-5) == 60


def test_nonnumeric_interval_defaults_600():
    r = _reping()
    assert r.reping_interval("xxx") == 600
    assert r.reping_interval("") == 600
    assert r.notify_key("b", 0, "xxx") == r.notify_key("b", 599, "xxx")
    assert r.notify_key("b", 0, "xxx") != r.notify_key("b", 600, "xxx")


def test_default_interval_is_600_floor_60():
    r = _reping()
    assert r.REPING_INTERVAL_DEFAULT_S == 600
    assert r.REPING_INTERVAL_FLOOR_S == 60


def test_key_carries_the_base_and_a_bucket_suffix():
    r = _reping()
    assert r.notify_key("network-reach-strih", 0, 600) == "network-reach-strih-0"


# --------------------------------------------------------------------- dante delegates, no 2nd impl
def test_dante_delegates_to_the_shared_twin():
    dc = _dante()
    r = _reping()
    for base, now, iv in [("dante-clock-cam1", 0, 600), ("dante-clock-strih", 601234, 600),
                          ("x", 120, 10), ("y", 5, "zzz")]:
        assert dc.dedup_key(base, now, iv) == r.notify_key(base, now, iv), (base, now, iv)
    assert dc.reping_interval(30) == r.reping_interval(30) == 60
    assert dc.reping_interval("zz") == r.reping_interval("zz") == 600


def test_dante_dedup_key_is_the_shared_function_object():
    # the delegation must be a true reuse, not a copy-pasted body -- pin that dante's dedup_key/
    # reping_interval resolve INTO watchdog_reping (a re-implemented copy would drift).
    dc = _dante()
    r = _reping()
    assert dc.reping_interval is r.reping_interval


# --------------------------------------------------------------------- python CLI mirror
def test_cli_dedup_key_matches_the_function():
    import sys
    r = _reping()
    out = subprocess.run(
        [sys.executable, str(_SCRIPTS / "watchdog_reping.py"),
         "dedup-key", "--base", "wd-strih", "--now", "601234", "--interval", "600"],
        capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    assert out.stdout.strip() == r.notify_key("wd-strih", 601234, 600)


# --------------------------------------------------------------------- bash <-> python PARITY
_PARITY_VECTOR = [
    ("wd-cam1", 0, "600"),
    ("wd-cam1", 599, "600"),
    ("wd-cam1", 600, "600"),
    ("wd-strih", 1783647854, "600"),
    ("wd-x", 120, "10"),      # clamped to 60
    ("wd-x", 180, "10"),      # clamped to 60, next bucket
    ("wd-y", 5, "zzz"),       # non-numeric -> 600 default
    ("wd-z", 1783647854, "300"),
]


def _bash_key(base, now, interval):
    """Source the shared bash helper in a subprocess and call watchdog_notify_key <base> <now> <interval>."""
    script = (
        'set -uo pipefail; . "$1" 2>/dev/null; '
        'watchdog_notify_key "$2" "$3" "$4"'
    )
    out = subprocess.run(
        ["bash", "-c", script, "bash", str(_LIB), base, str(now), str(interval)],
        capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    return out.stdout.strip()


def test_bash_python_parity_over_the_vector():
    r = _reping()
    for base, now, interval in _PARITY_VECTOR:
        py = r.notify_key(base, now, interval)
        sh = _bash_key(base, now, interval)
        assert py == sh, f"parity mismatch for ({base},{now},{interval}): python={py} bash={sh}"


def test_bash_env_default_interval_when_arg_omitted():
    """watchdog_notify_key <base> <now> (2-arg) reads $REPING_INTERVAL_S (default 600 when unset) --
    the one shared env name every production-critical watchdog passes the same way."""
    r = _reping()
    # env unset -> 600 default
    out = subprocess.run(
        ["bash", "-c", 'set -uo pipefail; . "$1"; unset REPING_INTERVAL_S; watchdog_notify_key "$2" "$3"',
         "bash", str(_LIB), "wd-cam2", "601234"],
        capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    assert out.stdout.strip() == r.notify_key("wd-cam2", 601234, 600)
    # env set -> that interval
    out2 = subprocess.run(
        ["bash", "-c", 'set -uo pipefail; . "$1"; export REPING_INTERVAL_S=300; watchdog_notify_key "$2" "$3"',
         "bash", str(_LIB), "wd-cam2", "601234"],
        capture_output=True, text=True)
    assert out2.returncode == 0, out2.stderr
    assert out2.stdout.strip() == r.notify_key("wd-cam2", 601234, 300)
