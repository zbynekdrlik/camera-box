"""issue 1441 -- both dev1 lipsync watchdogs loop the venue table (scripts/lib/avsync-venues.sh).

Per venue they read that venue's dev2 heartbeat (`avsync-watchdog-heartbeat-<venue>.txt`) and that
venue's stream box outputActive (its OBS-WS host from the table), keep per-venue state + dedup keys
(`avsync-<venue>-...`), and name the venue in every page. A venue whose stream box is away (the
obs-fleet traveling gate) is AWAY: nothing is read for it and it never pages.

Hermetic: a fake `ssh` (the dev2 transport) answers per venue from the remote command it is handed, a
fake obs_phase2 / notify / curl, OBS_FLEET_HOME pins which traveling box is home, no network.
"""

import os
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_HB_WATCHDOG = _ROOT / "scripts" / "avsync-heartbeat-alert-watchdog.sh"
_LINEUP_WATCHDOG = _ROOT / "scripts" / "avsync-lineup-alert-watchdog.sh"

FRESH_OK = "measured: db=-5.4 [2026-10-15 12:00:00] AV offset +0 fr (+0 ms) conf 8.2 :: A/V sync OK (offset 0 ms)"
FRESH_SILENT = "measured: db=-91.0 [2026-10-15 12:00:00] UNMEASURABLE window (best confidence 3.2 < 4.0 - no usable face)"
FRESH_OFFSET = "measured: db=-5.4 [2026-10-15 12:00:00] AV offset +2 fr (+80 ms) conf 8.1 :: audio predbieha video o ~80 ms -> ZNIZ 2ME PGM latency o 80"
STALE = "STALE"   # the fake ssh writes an epoch from 2001 for this one


def _exe(path, body):
    path.write_text(body)
    path.chmod(0o755)
    return path


class Rig:
    """A temp HOME + stub dir: fake ssh answering per venue, fake notify/obs_phase2/curl/systemctl."""

    def __init__(self, tmp_path, snv=FRESH_OK, pp=FRESH_OK):
        self.tmp = tmp_path
        self.stub = tmp_path / "stub"
        self.stub.mkdir()
        self.home = tmp_path / "home"
        self.home.mkdir()
        self.ssh_log = tmp_path / "ssh.log"
        self.notify_log = tmp_path / "notify.log"
        self.obs_log = tmp_path / "obs.log"
        self.curl_log = tmp_path / "curl.log"
        self.state = tmp_path / "state"
        (tmp_path / "hb-snv").write_text(snv)
        (tmp_path / "hb-pp").write_text(pp)
        _exe(
            self.stub / "ssh",
            "#!/usr/bin/env python3\n"
            "import sys, time, pathlib\n"
            "cmd = sys.argv[-1]\n"
            f"open({str(self.ssh_log)!r}, 'a').write(cmd + '\\n')\n"
            f"d = pathlib.Path({str(tmp_path)!r})\n"
            "if 'measurements-' in cmd:\n"
            "    sys.exit(0)\n"
            "venue = 'pp' if 'heartbeat-pp' in cmd else ('snv' if 'heartbeat-snv' in cmd else None)\n"
            "if venue:\n"
            "    body = (d / ('hb-' + venue)).read_text()\n"
            "    epoch = 1000000000 if body == 'STALE' else int(time.time())\n"
            "    print('%d\\t%s' % (epoch, body))\n"
            "print('---AVSYNC-HB-SEP---')\n",
        )
        self.notify = _exe(
            tmp_path / "fake_notify.py",
            f"import sys\nopen({str(self.notify_log)!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n",
        )
        self.obs_phase2 = _exe(
            tmp_path / "fake_obs_phase2.py",
            "import sys, os\n"
            "host = sys.argv[sys.argv.index('--host') + 1]\n"
            f"open({str(self.obs_log)!r}, 'a').write(host + '\\n')\n"
            "print('active=' + os.environ.get('FAKE_STREAM_ACTIVE', 'True') + ' path=')\n",
        )
        _exe(
            self.stub / "curl",
            f"#!/bin/sh\necho CURL >> '{self.curl_log}'\nprintf '%s\\n%s' '{{\"id\":\"1\"}}' \"${{FAKE_CURL_HTTP_CODE:-200}}\"\n",
        )
        _exe(self.stub / "systemctl", "#!/bin/sh\nexit 0\n")
        # issue 1441: the rig-mode read (cam2 painter) -- never the live rig. FAKE_RIG_MODE picks
        # the snapshot: EVENT (default, a production), TEST (development) or an empty answer.
        _exe(
            self.stub / "sshpass",
            "#!/bin/sh\n"
            'case "${FAKE_RIG_MODE:-EVENT}" in\n'
            "  EVENT) printf 'RIG_MODE_PROBE_OK\\nPID_PRESENT|0\\nPID_ALIVE|0\\nSVC_ENABLED|0\\nSVC_ACTIVE|0\\n' ;;\n"
            "  TEST) printf 'RIG_MODE_PROBE_OK\\nPID_PRESENT|1\\nPID_ALIVE|1\\nSVC_ENABLED|1\\nSVC_ACTIVE|1\\n' ;;\n"
            "esac\n",
        )
        self.discord_env = tmp_path / "discord.env"
        self.discord_env.write_text("DISCORD_BOT_TOKEN=fake-token\n")

    def env(self, home_boxes="stream-pp", extra=None):
        env = {
            "PATH": f"{self.stub}:/usr/bin:/bin",
            "HOME": str(self.home),
            "AVSYNC_HEARTBEAT_HOST": "dev2",
            "OBS_FLEET_HOME": home_boxes,
            "AIRULESET_NOTIFY": str(self.notify),
            "AVSYNC_DISCORD_ENV": str(self.discord_env),
            "AVSYNC_HEARTBEAT_STATE_FILE": str(self.state),
            "AVSYNC_REPORT_STATE_FILE": str(self.tmp / "report-state.json"),
            "AVSYNC_REPORT_FETCH_CMD": "true",
            "AVSYNC_LINEUP_STATE_FILE": str(self.state),
            "AVSYNC_LINEUP_OBS_PHASE2": str(self.obs_phase2),
            "AVSYNC_OBS_PHASE2": str(self.obs_phase2),
            "AVSYNC_LINEUP_SYSTEMCTL": str(self.stub / "systemctl"),
        }
        if extra:
            env.update(extra)
        return env

    def run(self, script, args=(), **kw):
        return subprocess.run(
            ["bash", str(script), *args], env=self.env(**kw), capture_output=True, text=True, timeout=120
        )

    def lines(self, path):
        return path.read_text().splitlines() if path.exists() else []

    def state_fields(self):
        return dict(l.split("=", 1) for l in self.lines(self.state) if "=" in l)


# ── the heartbeat watchdog ───────────────────────────────────────────────────────────────────────


def test_heartbeat_watchdog_reads_each_venues_own_heartbeat(tmp_path):
    rig = Rig(tmp_path)
    p = rig.run(_HB_WATCHDOG)
    assert p.returncode == 0, p.stderr
    cmds = rig.lines(rig.ssh_log)
    assert any("avsync-watchdog-heartbeat-snv.txt" in c for c in cmds), cmds
    assert any("avsync-watchdog-heartbeat-pp.txt" in c for c in cmds), cmds
    assert rig.lines(rig.notify_log) == [], "two fresh heartbeats must never page"


def test_heartbeat_watchdog_pages_the_stale_venue_only_and_names_it(tmp_path):
    rig = Rig(tmp_path, snv=FRESH_OK, pp=STALE)
    p = rig.run(_HB_WATCHDOG)
    assert p.returncode == 0, p.stderr
    calls = rig.lines(rig.notify_log)
    assert len(calls) == 1, calls
    assert "[PP]" in calls[0] and "stream-pp" in calls[0]
    # issue 1441: one page per incident -- a STABLE key, no time bucket.
    assert calls[0].endswith("--dedup-key avsync-pp-heartbeat-watchdog"), calls[0]
    fields = rig.state_fields()
    assert fields.get("pp_watchdog_confirm") == "1"
    assert fields.get("snv_watchdog_confirm") == "0"


def test_heartbeat_watchdog_snv_page_carries_the_snv_key(tmp_path):
    rig = Rig(tmp_path, snv=STALE, pp=FRESH_OK)
    rig.run(_HB_WATCHDOG)
    calls = rig.lines(rig.notify_log)
    assert len(calls) == 1 and "[SNV]" in calls[0]
    assert calls[0].endswith("--dedup-key avsync-snv-heartbeat-watchdog"), calls[0]


def test_heartbeat_watchdog_away_venue_is_never_read_nor_paged(tmp_path):
    rig = Rig(tmp_path, snv=FRESH_OK, pp=STALE)
    p = rig.run(_HB_WATCHDOG, home_boxes="nothing-is-home")
    assert p.returncode == 0, p.stderr
    assert "pp: AWAY" in p.stderr
    assert not any("heartbeat-pp" in c for c in rig.lines(rig.ssh_log))
    assert rig.lines(rig.notify_log) == []


def test_heartbeat_watchdog_report_pass_runs_per_venue_and_names_it(tmp_path):
    rig = Rig(tmp_path)
    # the fetch seam gets the venue as $1: only PP has a confident clip right now
    fetch = (
        'if [ "$1" = pp ]; then n=$(date +%s); for d in 180 90 0; do printf "%s\\t%s\\n" "$((n - d))" '
        '"measured: db=-5.0 [x] AV offset +0 fr (+0 ms) conf 6.0 :: A/V sync OK (offset 0 ms)"; done; fi'
    )
    p = rig.run(_HB_WATCHDOG, ["--dry-run"], extra={"AVSYNC_REPORT_FETCH_CMD": fetch})
    assert p.returncode == 0, p.stderr
    posts = [l for l in p.stderr.splitlines() if "WOULD post report message" in l]
    assert len(posts) == 1, p.stderr
    assert "[PP] 📐 A/V overené" in posts[0] and "(vysielanie)" in posts[0]


def test_heartbeat_watchdog_report_state_is_per_venue(tmp_path):
    rig = Rig(tmp_path)
    fetch = (
        'n=$(date +%s); for d in 180 90 0; do printf "%s\\t%s\\n" "$((n - d))" '
        '"measured: db=-5.0 [x] AV offset +0 fr (+0 ms) conf 6.0 :: A/V sync OK (offset 0 ms)"; done'
    )
    p = rig.run(_HB_WATCHDOG, extra={"AVSYNC_REPORT_FETCH_CMD": fetch})
    assert p.returncode == 0, p.stderr
    assert (tmp_path / "report-state-snv.json").exists()
    assert (tmp_path / "report-state-pp.json").exists()
    assert len(rig.lines(rig.curl_log)) == 2, "one first-verdict post per venue"


def test_heartbeat_watchdog_default_fetch_reads_the_venue_day_logs(tmp_path):
    rig = Rig(tmp_path)
    env_extra = {"AVSYNC_REPORT_FETCH_CMD": ""}
    p = rig.run(_HB_WATCHDOG, ["--dry-run"], extra=env_extra)
    assert p.returncode == 0, p.stderr
    fetches = [c for c in rig.lines(rig.ssh_log) if "measurements-" in c]
    assert any("measurements-snv-" in c for c in fetches), fetches
    assert any("measurements-pp-" in c for c in fetches), fetches


def test_heartbeat_watchdog_has_no_hand_typed_stream_address():
    assert "10.77.9.204" not in _HB_WATCHDOG.read_text()


# ── the lineup watchdog ──────────────────────────────────────────────────────────────────────────


def test_lineup_reads_each_venues_stream_state_from_the_table_host(tmp_path):
    rig = Rig(tmp_path)
    p = rig.run(_LINEUP_WATCHDOG, ["--dry-run"])
    assert p.returncode == 0, p.stderr
    assert rig.lines(rig.obs_log) == ["stream-pp.lan", "10.77.9.204"]
    cmds = rig.lines(rig.ssh_log)
    assert any("heartbeat-snv" in c for c in cmds) and any("heartbeat-pp" in c for c in cmds)


def test_lineup_pages_a_dead_pp_line_with_the_pp_key(tmp_path):
    rig = Rig(tmp_path, snv=FRESH_OK, pp=FRESH_SILENT)
    p = rig.run(_LINEUP_WATCHDOG)
    assert p.returncode == 0, p.stderr
    calls = rig.lines(rig.notify_log)
    assert len(calls) == 1, calls
    assert "[PP]" in calls[0] and "MRTVA" in calls[0]
    assert "--dedup-key avsync-pp-lineup-liveness" in calls[0]
    fields = rig.state_fields()
    assert fields.get("pp_lineup_confirm") == "1" and fields.get("snv_lineup_confirm") == "0"


def test_lineup_offset_page_is_per_venue_and_time_bucketed(tmp_path):
    rig = Rig(tmp_path, snv=FRESH_OK, pp=FRESH_OFFSET)
    rig.run(_LINEUP_WATCHDOG)
    calls = [c for c in rig.lines(rig.notify_log) if "ROZLADENIE" in c]
    assert len(calls) == 1, rig.lines(rig.notify_log)
    assert "[PP]" in calls[0] and "--dedup-key avsync-pp-offset-stream-" in calls[0]


def test_lineup_away_venue_is_never_read_nor_paged(tmp_path):
    rig = Rig(tmp_path, snv=FRESH_OK, pp=FRESH_SILENT)
    p = rig.run(_LINEUP_WATCHDOG, home_boxes="nothing-is-home")
    assert p.returncode == 0, p.stderr
    assert "pp: AWAY" in p.stderr
    assert rig.lines(rig.obs_log) == ["10.77.9.204"]
    assert rig.lines(rig.notify_log) == []


def test_lineup_assert_grades_every_home_venue_and_pings_discord_once(tmp_path):
    rig = Rig(tmp_path, snv=FRESH_OK, pp=FRESH_SILENT)
    p = rig.run(_LINEUP_WATCHDOG, ["--assert"])
    assert p.returncode == 1, p.stdout + p.stderr
    out = p.stdout.splitlines()
    assert any(l.startswith("[SNV] GO") for l in out), out
    assert any(l.startswith("[PP] NO-GO") for l in out), out
    assert len(rig.lines(rig.curl_log)) == 1, "one Discord test-ping per assert, not one per venue"
    nogo = [c for c in rig.lines(rig.notify_log) if "PRE-EVENT NO-GO" in c]
    assert len(nogo) == 1 and "[PP]" in nogo[0]
    assert "--dedup-key avsync-pp-lineup-preflight-nogo" in nogo[0]


def test_lineup_assert_go_when_every_home_venue_is_go(tmp_path):
    rig = Rig(tmp_path)
    p = rig.run(_LINEUP_WATCHDOG, ["--assert"])
    assert p.returncode == 0, p.stdout + p.stderr
    assert not any("NO-GO" in l for l in p.stdout.splitlines())


def test_lineup_has_no_hand_typed_stream_address():
    assert "10.77.9.204" not in _LINEUP_WATCHDOG.read_text()


def test_avsync_venues_narrows_both_watchdogs(tmp_path):
    rig = Rig(tmp_path)
    p = rig.run(_LINEUP_WATCHDOG, ["--dry-run"], extra={"AVSYNC_VENUES": "pp"})
    assert p.returncode == 0, p.stderr
    assert rig.lines(rig.obs_log) == ["stream-pp.lan"]


# ── issue 1441: development never pages ──────────────────────────────────────────────────────────


def test_heartbeat_watchdog_stale_snv_in_test_mode_never_pages(tmp_path):
    rig = Rig(tmp_path, snv=STALE, pp=FRESH_OK)
    p = rig.run(_HB_WATCHDOG, extra={"FAKE_RIG_MODE": "TEST"})
    assert p.returncode == 0, p.stderr
    assert rig.lines(rig.notify_log) == []
    assert "snv watchdog: heartbeat CONFIRMED stale, but no production (development)" in p.stderr


def test_heartbeat_watchdog_stale_pp_while_it_does_not_stream_never_pages(tmp_path):
    rig = Rig(tmp_path, snv=FRESH_OK, pp=STALE)
    p = rig.run(_HB_WATCHDOG, extra={"FAKE_STREAM_ACTIVE": "False"})
    assert p.returncode == 0, p.stderr
    assert rig.lines(rig.notify_log) == []
    assert any("stream-pp" in h for h in rig.lines(rig.obs_log)), rig.lines(rig.obs_log)


def test_heartbeat_watchdog_unreadable_rig_mode_still_pages_a_streaming_snv(tmp_path):
    rig = Rig(tmp_path, snv=STALE, pp=FRESH_OK)
    rig.run(_HB_WATCHDOG, extra={"FAKE_RIG_MODE": "NONE"})
    calls = rig.lines(rig.notify_log)
    assert len(calls) == 1 and "[SNV]" in calls[0]


def test_lineup_watchdog_silent_snv_line_in_test_mode_never_pages(tmp_path):
    rig = Rig(tmp_path, snv=FRESH_SILENT, pp=FRESH_OK)
    p = rig.run(_LINEUP_WATCHDOG, extra={"FAKE_RIG_MODE": "TEST"})
    assert p.returncode == 0, p.stderr
    assert not any("[SNV]" in c for c in rig.lines(rig.notify_log)), rig.lines(rig.notify_log)
    assert "snv: ALARM (no-audio) during development (no production) -- never a page" in p.stderr


def test_lineup_watchdog_offset_in_test_mode_never_pages(tmp_path):
    rig = Rig(tmp_path, snv=FRESH_OFFSET, pp=FRESH_OK)
    p = rig.run(_LINEUP_WATCHDOG, extra={"FAKE_RIG_MODE": "TEST"})
    assert p.returncode == 0, p.stderr
    assert not any("[SNV]" in c for c in rig.lines(rig.notify_log))
    assert "snv offset: ALARM" in p.stderr and "never a page" in p.stderr


def test_lineup_watchdog_silent_snv_line_in_event_mode_pages(tmp_path):
    rig = Rig(tmp_path, snv=FRESH_SILENT, pp=FRESH_OK)
    rig.run(_LINEUP_WATCHDOG, extra={"FAKE_RIG_MODE": "EVENT"})
    assert any("[SNV]" in c for c in rig.lines(rig.notify_log))
