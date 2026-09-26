"""#1313 -- the dev1 `local` node ARM of the dante-clock alert watchdog, driven end-to-end.

WHY: dev1 is NOT a probed cam/obs node, yet it runs dantesync and its clock feeds every dev1-hosted
gate (clock-offset-painter-gate.sh, the recording-verdict wall references, every date-stamped E2E
window). On 14.9.2026 dev1 sat NTP-only for ~a day unpaged (its gm_allowlist on the retired literal
+ a fleet roll that skipped it) -- the dev1 watchdog that would have paged it runs ON dev1 and never
looked at 127.0.0.1:8898. This closes that blind spot: a `DANTE_CLOCK_LOCAL_NODES="dev1"` arm probes
the loopback :8898 with NO ssh/TCP reach probe (the box is by definition up), graded with the SAME
verdicts, production-critical TIME-BUCKETED dedup key `dante-clock-dev1-<bucket>`.

These tests drive the REAL orchestrator scripts/dantesync-clock-alert-watchdog.sh in --dry-run via
the DANTE_CLOCK_* seams (fetch stub + inline env), NO live rig -- the sibling
test_cg_bridge_alert_watchdog / test_ndi_portmap_watchdog bash-driving pattern. Run under pytest
(subprocess bash inside the python process, so the worktree-isolation guard never sees a `bash -c`).
The pure-decision half (analyze_local / --local) is in test_dantesync_clock_decision_1307.py.
"""
import importlib.util
import os
import pathlib
import stat
import subprocess
import tempfile

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_WATCHDOG = _ROOT / "scripts" / "dantesync-clock-alert-watchdog.sh"

# reuse the pure module to compute the EXPECTED time-bucketed dedup key (never hardcode the bucket).
_MOD = _ROOT / "scripts" / "dantesync_clock_decision.py"
_spec = importlib.util.spec_from_file_location("dantesync_clock_decision", _MOD)
dc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(dc)

_NOW = 1789379394
_INTERVAL = 600
GM = "10.77.9.230"


def _status(is_locked="true", mode="LOCK", gm=GM, storm="false", updated_ts=_NOW):
    parts = [
        '"offset_ns":1', '"settled":true', f'"updated_ts":{updated_ts}',
        f'"is_locked":{is_locked}', f'"mode":"{mode}"', f'"gm_source_ip":"{gm}"',
        f'"ntp_step_storm":{storm}', '"ntp_steps_last_hour":null',
    ]
    return "{" + ",".join(parts) + "}"


def _run_dry(tmp, fetch_stdout=None, fetch_rc=0):
    """Run the watchdog one --dry-run pass with ONLY dev1 in the roster, a stubbed :8898 fetch, a
    fixed grandmaster + now, and CONFIRM_THRESHOLD=1 so a fault pages on the first (only) pass.
    Returns the pass's stderr (where --dry-run logs `[dry-run] WOULD alert (dedup-key=...)`)."""
    d = pathlib.Path(tmp)
    fetch = d / "fetch.sh"
    body = "" if fetch_stdout is None else fetch_stdout
    fetch.write_text(
        "#!/usr/bin/env bash\n"
        + (f"cat <<'JSON'\n{body}\nJSON\n" if fetch_stdout is not None else "")
        + f"exit {fetch_rc}\n"
    )
    fetch.chmod(fetch.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    env = dict(os.environ)
    env.update({
        "DANTE_CLOCK_LOCAL_NODES": "dev1",
        "DANTE_CLOCK_CAM_NODES": "",       # no cams -> only dev1 in the roster
        "DANTE_CLOCK_OBS_NODES": "",       # no obs boxes either
        "DANTE_CLOCK_FIXED_NODES": "",     # issue 1372: nor the audio-VLAN PCs (mbc, fohabl)
        "DANTE_CLOCK_FETCH_CMD": str(fetch),
        "RIG_GRANDMASTER_IP": GM,          # neutralize the DNS / GM-move global pages
        "DANTE_CLOCK_CONFIRM_THRESHOLD": "1",
        "DANTE_CLOCK_NOW": str(_NOW),
        "DANTE_CLOCK_REPING_INTERVAL_S": str(_INTERVAL),
        "DANTE_CLOCK_VERSION_PIN": "1.8.53",  # report-only; avoids sourcing the version gate
        "DANTE_CLOCK_ALERT_STATE_DIR": str(d),
    })
    r = subprocess.run(["bash", str(_WATCHDOG), "--dry-run"], capture_output=True, text=True, env=env)
    assert r.returncode == 0, f"watchdog dry-run exited {r.returncode}\nSTDERR:\n{r.stderr}"
    return r.stderr


# ------------------------------------------------------------------ dev1 NO_CLOCK -> pages
def test_dev1_no_clock_pages_with_bucketed_dev1_key():
    with tempfile.TemporaryDirectory() as tmp:
        err = _run_dry(tmp, fetch_stdout=_status(is_locked="false", mode="ACQ"))
    key = dc.dedup_key("dante-clock-dev1", _NOW, _INTERVAL)
    assert key == "dante-clock-dev1-2982298", key  # floor(1789379394/600)
    assert "WOULD alert" in err, err
    assert f"dedup-key={key}" in err, err
    # the alert is the dev1 node's own clock-loss page.
    assert "dev1" in err and "STRATIL dante clock" in err, err


# ------------------------------------------------------------------ dev1 :8898 dead -> NO_DANTESYNC
def test_dev1_dead_8898_pages_no_dantesync_never_skip():
    # THE local difference: dev1 is up by definition, so a dead :8898 is NO_DANTESYNC (daemon
    # crashed on a live box), never the remote-node SKIP-and-defer-#1001 hold.
    with tempfile.TemporaryDirectory() as tmp:
        err = _run_dry(tmp, fetch_stdout=None, fetch_rc=1)  # curl-fail stub -> unreachable :8898
    key = dc.dedup_key("dante-clock-nohttp-dev1", _NOW, _INTERVAL)
    assert "WOULD alert" in err, err
    assert f"dedup-key={key}" in err, err
    assert "NEODPOVEDÁ" in err, err  # the NO_DANTESYNC alert body
    # it must NOT have SKIPped (the remote-node down-box path) for dev1.
    assert "box/:8898-down is #1001 territory" not in err, err


# ------------------------------------------------------------------ dev1 healthy -> no page at all
def test_dev1_healthy_pages_nothing():
    with tempfile.TemporaryDirectory() as tmp:
        err = _run_dry(tmp, fetch_stdout=_status())  # locked, LOCK, correct gm, fresh
    # no node page, and RIG_GRANDMASTER_IP set + gm matches => no DNS / GM-move page either.
    assert "WOULD alert" not in err, err
    # sanity: the pass actually ran the dev1 node and graded it OK.
    assert "dev1" in err and "verdict=OK" in err, err
