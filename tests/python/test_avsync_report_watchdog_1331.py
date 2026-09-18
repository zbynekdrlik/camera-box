"""#1331 -- the dev1 watchdog `report` pass (scripts/avsync-heartbeat-alert-watchdog.sh) that
REPLACES the raw one-clip forward (maybe_forward_verdict) with the verified-session report.

Runs the real watchdog script under `--dry-run` with the row-fetch STUBBED via
`AVSYNC_REPORT_FETCH_CMD` (the same env-seam shape the heartbeat host tests use), the heartbeat legs
neutralized (an unknown host = empty probe = skipped, no network), and asserts the report pass would
POST a verified-session message and that the OLD `📐 A/V-sync meranie:` forward is gone.
"""

import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_HOME = str(pathlib.Path.home())
_WATCHDOG = _ROOT / "scripts" / "avsync-heartbeat-alert-watchdog.sh"


def _run_dry_run(env_extra, tmp_path):
    env = {
        "PATH": "/usr/bin:/bin",
        "HOME": _HOME,
        # An unknown host => avsync_heartbeat_ssh_prefix_argv returns nothing => empty probe =>
        # the heartbeat legs are skipped, no ssh/network. The report pass still runs.
        "AVSYNC_HEARTBEAT_HOST": "none",
        "AVSYNC_HEARTBEAT_STATE_FILE": str(tmp_path / "hb.state"),
        "AVSYNC_REPORT_STATE_FILE": str(tmp_path / "report-state.json"),
    }
    env.update(env_extra)
    proc = subprocess.run(
        ["bash", str(_WATCHDOG), "--dry-run"],
        env=env,
        capture_output=True,
        text=True,
    )
    return proc


def test_report_pass_would_post_a_verified_session_message(tmp_path):
    # One CONFIDENT clip at "now" => a live session => the START ("📐 A/V overené ... začiatok
    # vysielania") message. --dry-run must LOG a "WOULD post" instead of actually POSTing.
    fetch = (
        "printf '%s\\t%s\\n' \"$(date +%s)\" "
        "\"measured: db=-5.0 [2026-09-17 19:00:59] AV offset +3 fr (+120 ms) conf 5.4 "
        ":: audio predbieha video o ~120 ms -> ZNIZ '2ME PGM' latency o 120\""
    )
    proc = _run_dry_run({"AVSYNC_REPORT_FETCH_CMD": fetch}, tmp_path)
    assert proc.returncode == 0, proc.stderr
    assert "WOULD post" in proc.stderr, proc.stderr
    assert "📐 A/V overené" in proc.stderr, proc.stderr
    # the OLD raw one-clip forward is GONE
    assert "📐 A/V-sync meranie:" not in proc.stderr


def test_report_pass_no_rows_is_a_quiet_noop(tmp_path):
    # An empty fetch (no rows) must post nothing and never crash the pass.
    proc = _run_dry_run({"AVSYNC_REPORT_FETCH_CMD": "true"}, tmp_path)
    assert proc.returncode == 0, proc.stderr
    assert "WOULD post" not in proc.stderr


def test_maybe_forward_verdict_is_removed_and_report_pass_replaces_it():
    src = _WATCHDOG.read_text()
    assert "maybe_forward_verdict" not in src, "the raw one-clip forward must be removed"
    assert "run_report_pass" in src, "the report pass must replace it"
    assert "post_discord_verdict" in src, "post_discord_verdict is reused by the report pass"


def test_forwardable_lib_helpers_are_removed():
    # The forward-only lib helpers have no remaining caller once the forward is gone.
    lib = (_ROOT / "scripts" / "lib" / "avsync-heartbeat.sh").read_text()
    assert "avsync_heartbeat_is_forwardable_verdict" not in lib
    assert "avsync_heartbeat_verdict_signature" not in lib


# ── #1331 F1/F2: transactional delivery -- commit report state ONLY after Discord delivery ───────

import json as _json
import os as _os
import stat as _stat

_START_FETCH = (
    "printf '%s\\t%s\\n' \"$(date +%s)\" "
    "\"measured: db=-5.0 [2026-09-17 19:00:59] AV offset +3 fr (+120 ms) conf 5.4 "
    ":: audio predbieha video o ~120 ms -> ZNIZ '2ME PGM' latency o 120\""
)


def _run_live(tmp_path, curl_http_code, fetch):
    """Run the watchdog NON-dry-run with a fake `curl` (returns curl_http_code) + a fixture Discord
    env carrying a token, so post_discord_verdict actually attempts a POST. Returns (proc, state_path,
    curl_marker)."""
    binp = tmp_path / "bin"
    binp.mkdir()
    curl = binp / "curl"
    marker = tmp_path / "curl-calls.log"
    # Mimic real `curl -sS -w '\n%{http_code}'`: print body + newline + the forced http code.
    curl.write_text(
        "#!/bin/sh\n"
        'echo "CURL $*" >> "%s"\n' % marker
        + "printf '%%s\\n%%s' '{\"id\":\"1\"}' '%s'\n" % curl_http_code
        + "exit 0\n"
    )
    curl.chmod(curl.stat().st_mode | _stat.S_IEXEC | _stat.S_IXGRP | _stat.S_IXOTH)

    disc_env = tmp_path / "discord.env"
    disc_env.write_text("DISCORD_BOT_TOKEN=faketoken\n")

    state_path = tmp_path / "report-state.json"
    env = {
        "PATH": f"{binp}:/usr/bin:/bin",
        "HOME": _HOME,
        "AVSYNC_HEARTBEAT_HOST": "none",
        "AVSYNC_HEARTBEAT_STATE_FILE": str(tmp_path / "hb.state"),
        "AVSYNC_REPORT_STATE_FILE": str(state_path),
        "AVSYNC_DISCORD_ENV": str(disc_env),
        "AVSYNC_REPORT_FETCH_CMD": fetch,
    }
    proc = subprocess.run(
        ["bash", str(_WATCHDOG)], env=env, capture_output=True, text=True
    )
    return proc, state_path, marker


def test_state_is_committed_only_after_a_successful_discord_post(tmp_path):
    proc, state_path, marker = _run_live(tmp_path, "200", _START_FETCH)
    assert proc.returncode == 0, proc.stderr
    assert marker.exists() and "CURL" in marker.read_text(), "a POST must have been attempted"
    assert state_path.exists(), "on a successful post the advanced report state must be committed"
    st = _json.loads(state_path.read_text())
    assert st["start_posted"] is True


def test_state_is_NOT_committed_when_the_discord_post_fails(tmp_path):
    # A transient POST failure (HTTP 500) must NOT advance the dedup state -- else the message is
    # permanently lost (the exact "missing verified-A/V messages" failure this ticket fixes).
    proc, state_path, marker = _run_live(tmp_path, "500", _START_FETCH)
    assert proc.returncode == 0, proc.stderr  # non-fatal: the pass survives
    assert marker.exists() and "CURL" in marker.read_text(), "a POST must have been attempted"
    if state_path.exists():
        st = _json.loads(state_path.read_text())
        assert st.get("start_posted") is not True, (
            "a failed post must NOT commit start_posted -- the next pass must re-emit the message"
        )


def test_dry_run_never_mutates_the_production_report_state(tmp_path):
    # A manual --dry-run during a live must not consume the real dedup state.
    proc = _run_dry_run({"AVSYNC_REPORT_FETCH_CMD": _START_FETCH}, tmp_path)
    assert proc.returncode == 0, proc.stderr
    assert "WOULD post" in proc.stderr
    state_path = tmp_path / "report-state.json"
    assert not state_path.exists(), "--dry-run must never create/mutate the production report state"
