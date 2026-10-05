"""issue 1406 -- the dev1 obs-handles alert watchdog end to end: a `--dry-run` replay through its
`OBS_HANDLES_FETCH_CMD` seam over the 4./5.10.2026 stream readings recorded on the ticket, plus
its fleet wiring (the obs-fleet `obs-handles` facet, the dev1 timer roster, the DISABLED units).

Each pass is one real run of scripts/obs-handles-alert-watchdog.sh with the pass time pinned by
`OBS_HANDLES_NOW_EPOCH` and the bundle-state body served by a fixture script, so the watchdog's
own state file carries the reference sample and the confirm counter from pass to pass. Tier-0:
bash + python3 subprocesses, no network (the fetch seam replaces curl; stream and strih-lx are
`always` boxes, resolume's home check is pinned by `OBS_FLEET_HOME`).
"""
from __future__ import annotations

import json
import os
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_WATCHDOG = _SCRIPTS / "obs-handles-alert-watchdog.sh"
_READINGS = json.loads(
    (_ROOT / "tests" / "fixtures" / "obs_handles_1406" / "stream-5-10-readings.json")
    .read_text(encoding="utf-8"))
_PASS_S = 300


def _body(handles, pid, start, limit=""):
    body = dict(_READINGS["base"])
    body.update({"obs_handles": str(handles), "obs_handles_pid": pid, "obs_handles_start": start})
    if limit:
        body["obs_handles_limit"] = limit
    return json.dumps(body)


class _Replay:
    """One box's sequence of watchdog passes over a scratch state file."""

    def __init__(self, tmp_path, boxes="stream|10.77.9.204", extra_env=None, dry_run=True):
        self.tmp = tmp_path
        self.body = tmp_path / "body.json"
        self.fetched = tmp_path / "fetched"
        fetch = tmp_path / "fetch.sh"
        fetch.write_text(
            "#!/usr/bin/env bash\n"
            f"echo \"$1\" >> '{self.fetched}'\n"
            f"[ -f '{self.body}' ] || exit 1\n"
            f"cat '{self.body}'\n", encoding="utf-8")
        fetch.chmod(0o755)
        self.env = dict(os.environ)
        self.env.update({
            "OBS_HANDLES_FETCH_CMD": str(fetch),
            "OBS_HANDLES_BOXES": boxes,
            "OBS_HANDLES_ALERT_STATE_FILE": str(tmp_path / "state"),
            "OBS_FLEET_HOME": "strih-lx stream",
        })
        self.env.pop("REPING_INTERVAL_S", None)
        self.env.update(extra_env or {})
        self.dry_run = dry_run

    def run(self, now, body):
        if body is None:
            self.body.unlink(missing_ok=True)
        else:
            self.body.write_text(body, encoding="utf-8")
        env = dict(self.env, OBS_HANDLES_NOW_EPOCH=str(now))
        args = ["bash", str(_WATCHDOG)] + (["--dry-run"] if self.dry_run else [])
        r = subprocess.run(args, capture_output=True, text=True, env=env, timeout=60)
        assert r.returncode == 0, r.stderr
        return r.stderr


def _leak_pass(i, t0):
    leak = _READINGS["leak"]
    return t0 + _PASS_S * i, _body(int(leak["start_handles"] + leak["per_s"] * _PASS_S * i),
                                    leak["pid"], leak["start"])


def test_the_5_10_leak_from_a_fresh_obs_pages_after_three_growing_passes(tmp_path):
    rp = _Replay(tmp_path)
    t0 = int(_READINGS["leak"]["start"]) + 60
    logs = [rp.run(*_leak_pass(i, t0)) for i in range(4)]
    assert "verdict=BASELINE" in logs[0]
    for i in (1, 2):
        assert "verdict=GROWING" in logs[i] and "WOULD alert" not in logs[i], logs[i]
    assert "verdict=GROWING" in logs[3]
    assert "[dry-run] WOULD alert (GROWING): stream" in logs[3]
    assert "rate=168744/h" in logs[3]


def test_the_5_10_census_snapshot_pages_on_the_ceiling(tmp_path):
    leak = _READINGS["leak"]
    rp = _Replay(tmp_path)
    t = 1_791_190_000
    per_pass = leak["census_growth_per_25s"] * _PASS_S // 25
    first = rp.run(t, _body(leak["census_handles"], leak["pid"], leak["start"]))
    assert "verdict=CEILING" in first and "WOULD alert" not in first
    second = rp.run(t + _PASS_S, _body(leak["census_handles"] + per_pass, leak["pid"],
                                        leak["start"]))
    assert "[dry-run] WOULD alert (CEILING): stream" in second
    assert "handles=4080836" in second


def test_the_post_fix_flat_readings_never_page(tmp_path):
    fix = _READINGS["post_fix"]
    rp = _Replay(tmp_path)
    logs = [rp.run(1_791_197_133 + _PASS_S * i, _body(h, fix["pid"], fix["start"]))
            for i, (_t, h) in enumerate(fix["samples"])]
    assert "verdict=BASELINE" in logs[0]
    assert all("verdict=HEALTHY" in log for log in logs[1:]), logs
    assert not any("WOULD alert" in log for log in logs)


def test_the_restart_after_a_page_logs_a_recovery_on_the_machine_channel(tmp_path):
    leak, fix = _READINGS["leak"], _READINGS["post_fix"]
    rp = _Replay(tmp_path)
    t = 1_791_190_000
    rp.run(t, _body(leak["census_handles"], leak["pid"], leak["start"]))
    paged = rp.run(t + _PASS_S, _body(leak["census_handles"] + 14_064, leak["pid"], leak["start"]))
    assert "WOULD alert (CEILING)" in paged
    restarted = rp.run(t + 2 * _PASS_S, _body(5806, fix["pid"], fix["start"]))
    assert "verdict=BASELINE" in restarted
    assert "RECOVERY: stream" in restarted
    again = rp.run(t + 3 * _PASS_S, _body(5790, fix["pid"], fix["start"]))
    assert "verdict=HEALTHY" in again and "RECOVERY" not in again


def test_a_manual_run_between_timer_passes_neither_pages_nor_resets(tmp_path):
    rp = _Replay(tmp_path)
    t0 = int(_READINGS["leak"]["start"]) + 60
    assert "BASELINE" in rp.run(*_leak_pass(0, t0))
    assert "GROWING" in rp.run(*_leak_pass(1, t0))
    leak = _READINGS["leak"]
    held = rp.run(t0 + _PASS_S + 60, _body(int(leak["start_handles"] + leak["per_s"] * 360),
                                            leak["pid"], leak["start"]))
    assert "verdict=HOLD" in held and "WOULD alert" not in held
    assert "WOULD alert" not in rp.run(*_leak_pass(2, t0))
    assert "WOULD alert (GROWING)" in rp.run(*_leak_pass(3, t0))


def test_unfetchable_is_skip_and_absent_facet_is_unknown(tmp_path):
    rp = _Replay(tmp_path)
    skip = rp.run(1000, None)
    assert "verdict=SKIP" in skip and "WOULD alert" not in skip
    unknown = rp.run(1300, json.dumps(_READINGS["base"]))
    assert "verdict=UNKNOWN" in unknown and "WOULD alert" not in unknown


def test_strih_lx_pages_near_its_own_open_files_limit(tmp_path):
    rp = _Replay(tmp_path, boxes="strih-lx|10.77.9.202")
    rp.run(1000, _body(850, "4242", "1791190000", limit="1024"))
    log = rp.run(1300, _body(851, "4242", "1791190000", limit="1024"))
    assert "[dry-run] WOULD alert (CEILING): strih-lx" in log
    assert "ceiling=819" in log


def test_resolume_is_polled_only_while_home(tmp_path):
    (tmp_path / "away").mkdir()
    away = _Replay(tmp_path / "away", boxes="resolume|resolume.lan")
    log = away.run(1000, _body(5000, "1", "2"))
    assert "away" in log and not away.fetched.exists()
    (tmp_path / "home").mkdir()
    home = _Replay(tmp_path / "home", boxes="resolume|resolume.lan",
                   extra_env={"OBS_FLEET_HOME": "resolume"})
    log = home.run(1000, _body(5000, "1", "2"))
    assert home.fetched.read_text().split() == ["resolume.lan"]
    assert "verdict=BASELINE" in log


def test_a_real_pass_pings_with_a_time_bucketed_key(tmp_path):
    notify = tmp_path / "notify.py"
    sent = tmp_path / "sent.json"
    notify.write_text(
        "import json, sys\n"
        f"with open({str(sent)!r}, 'a') as f:\n"
        "    f.write(json.dumps(sys.argv[1:]) + '\\n')\n", encoding="utf-8")
    rp = _Replay(tmp_path, extra_env={"AIRULESET_NOTIFY": str(notify)}, dry_run=False)
    leak = _READINGS["leak"]
    t = 1_791_190_000
    rp.run(t, _body(leak["census_handles"], leak["pid"], leak["start"]))
    rp.run(t + _PASS_S, _body(leak["census_handles"] + 14_064, leak["pid"], leak["start"]))
    calls = [json.loads(line) for line in sent.read_text().splitlines()]
    assert len(calls) == 1
    argv = calls[0]
    assert argv[0] == "notify"
    key = argv[argv.index("--dedup-key") + 1]
    assert key == f"obs-handles-stream-{(t + _PASS_S) // 600}"
    body = argv[argv.index("--body") + 1]
    assert body.startswith("🚨") and "stream" in body and "4080836" in body


def test_the_fleet_facet_and_the_watchdog_roster():
    r = subprocess.run(["bash", "-c", f'. "{_SCRIPTS}/lib/obs-fleet.sh"; '
                        "obs_fleet_facet_members obs-handles; echo; obs_fleet_boxes obs-handles"],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    members, boxes = r.stdout.splitlines()
    assert members == "strih-lx stream resolume"
    assert boxes == "strih-lx|10.77.9.202 stream|10.77.9.204 resolume|resolume.lan"
    r = subprocess.run(["bash", "-c", f'. "{_SCRIPTS}/lib/obs-fleet.sh"; '
                        "obs_fleet_facet_members nope"], capture_output=True, text=True)
    assert r.returncode != 0 and "obs-handles" in r.stderr
    src = _WATCHDOG.read_text(encoding="utf-8")
    assert 'BOXES="${OBS_HANDLES_BOXES:-$(obs_fleet_boxes obs-handles)}"' in src
    assert "obs_fleet_poll_now" in src
    roster = (_SCRIPTS / "lib" / "watchdog-roster.sh").read_text(encoding="utf-8")
    assert "obs-handles-alert-watchdog.timer:core" in roster


def test_the_units_ship_and_point_at_the_watchdog():
    svc = (_ROOT / "systemd" / "obs-handles-alert-watchdog.service").read_text(encoding="utf-8")
    tmr = (_ROOT / "systemd" / "obs-handles-alert-watchdog.timer").read_text(encoding="utf-8")
    assert "ExecStart=%h/devel/camera-box/scripts/obs-handles-alert-watchdog.sh" in svc
    assert "Type=oneshot" in svc
    assert "OnUnitActiveSec=5min" in tmr
    assert (_ROOT / "systemd" / "obs-handles-alert-watchdog.README.md").exists()


def test_the_watchdog_passes_bash_syntax_check():
    r = subprocess.run(["bash", "-n", str(_WATCHDOG)], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
