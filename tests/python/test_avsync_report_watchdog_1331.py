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
