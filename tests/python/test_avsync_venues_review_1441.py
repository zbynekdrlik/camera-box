"""issue 1441 review round 1 -- the per-venue lipsync watchdogs, the rollout and failure edges.

1. The verified-A/V REPORT reads only dev2's day logs, so it runs for an AWAY venue too: a traveling
   PP box shut down right after an event must still get its END report on time.
2. The legacy venue (snv) reads its per-venue heartbeat and falls back to the old single path, so dev1
   picking up the new watchdogs before dev2 runs the template instances never pages SNV stale. The
   SNV report reads the old day log too.
3. A venue table that lost the legacy venue, or came out empty, PAGES (stable key
   `avsync-venue-table`) -- a production-critical alarm never fails open by silently watching nothing.
4. The pre-issue-1441 report state moves to the legacy venue's file once (no START re-post on deploy).
5. AVSYNC_VENUES is never glob-expanded.
6. The venue table is derived once per pass: its warnings are printed once, not per accessor call.

The fake dev2 `ssh` here RUNS the remote command with HOME pointed at a fake dev2 home, so the real
`cat` / fallback logic is what is tested.
"""

import json
import pathlib
import subprocess
import time

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_HB = _ROOT / "scripts" / "avsync-heartbeat-alert-watchdog.sh"
_LINEUP = _ROOT / "scripts" / "avsync-lineup-alert-watchdog.sh"
_VENUES = _ROOT / "scripts" / "lib" / "avsync-venues.sh"

OK = "measured: db=-5.4 [x] AV offset +0 fr (+0 ms) conf 8.2 :: A/V sync OK (offset 0 ms)"


def _exe(path, body):
    path.write_text(body)
    path.chmod(0o755)
    return path


class Dev2:
    def __init__(self, tmp):
        self.tmp = tmp
        self.home = tmp / "dev2home"
        (self.home / ".camera-box").mkdir(parents=True)
        (self.home / "avsync").mkdir()
        self.stub = tmp / "stub"
        self.stub.mkdir()
        self.notify_log = tmp / "notify.log"
        self.curl_log = tmp / "curl.log"
        self.obs_log = tmp / "obs.log"
        # the fake dev2 transport: run the remote command for real, HOME = the fake dev2 home
        _exe(self.stub / "ssh", f'#!/bin/bash\nexec env HOME="{self.home}" bash -c "${{@: -1}}"\n')
        _exe(
            self.stub / "curl",
            f"#!/bin/sh\necho CURL >> '{self.curl_log}'\nprintf '%s\\n%s' '{{\"id\":\"1\"}}' 200\n",
        )
        self.notify = _exe(
            tmp / "notify.py",
            f"import sys\nopen({str(self.notify_log)!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n",
        )
        self.obs = _exe(
            tmp / "obs.py",
            "import sys\n"
            f"open({str(self.obs_log)!r}, 'a').write(sys.argv[sys.argv.index('--host') + 1] + '\\n')\n"
            "print('active=True path=')\n",
        )
        (tmp / "discord.env").write_text("DISCORD_BOT_TOKEN=t\n")

    def heartbeat(self, name, status=OK, age=0):
        (self.home / ".camera-box" / name).write_text(f"{int(time.time()) - age}\t{status}\n")

    def env(self, extra=None, home_boxes="nothing-is-home"):
        env = {
            "PATH": f"{self.stub}:/usr/bin:/bin",
            "HOME": str(self.tmp / "dev1home"),
            "AVSYNC_HEARTBEAT_HOST": "dev2",
            "OBS_FLEET_HOME": home_boxes,
            "AIRULESET_NOTIFY": str(self.notify),
            "AVSYNC_DISCORD_ENV": str(self.tmp / "discord.env"),
            "AVSYNC_HEARTBEAT_STATE_FILE": str(self.tmp / "state"),
            "AVSYNC_LINEUP_STATE_FILE": str(self.tmp / "state"),
            "AVSYNC_REPORT_STATE_FILE": str(self.tmp / "report-state.json"),
            "AVSYNC_REPORT_FETCH_CMD": "true",
            "AVSYNC_LINEUP_OBS_PHASE2": str(self.obs),
        }
        if extra:
            env.update(extra)
        return env

    def run(self, script, args=(), cwd=None, **kw):
        return subprocess.run(
            ["bash", str(script), *args], env=self.env(**kw), capture_output=True, text=True,
            timeout=120, cwd=cwd,
        )

    def notifies(self):
        return self.notify_log.read_text().splitlines() if self.notify_log.exists() else []

    def curls(self):
        return self.curl_log.read_text().splitlines() if self.curl_log.exists() else []


# ── 1. the report runs for an AWAY venue ─────────────────────────────────────────────────────────


def test_report_pass_runs_for_an_away_venue(tmp_path):
    d = Dev2(tmp_path)
    d.heartbeat("avsync-watchdog-heartbeat-snv.txt")
    fetch = (
        'if [ "$1" = pp ]; then printf "%s\\t%s\\n" "$(date +%s)" '
        '"measured: db=-5.0 [x] AV offset +0 fr (+0 ms) conf 6.0 :: A/V sync OK (offset 0 ms)"; fi'
    )
    p = d.run(_HB, ["--dry-run"], extra={"AVSYNC_REPORT_FETCH_CMD": fetch})   # PP box away
    assert p.returncode == 0, p.stderr
    assert "pp: AWAY" in p.stderr
    assert "WOULD post report message: [PP] 📐 A/V overené" in p.stderr
    assert "pp watchdog: last_heartbeat_epoch" not in p.stderr, "the heartbeat legs stay gated"


# ── 2. the legacy venue falls back to the old single heartbeat / day log ─────────────────────────


def test_snv_heartbeat_falls_back_to_the_old_single_file(tmp_path):
    d = Dev2(tmp_path)
    d.heartbeat("avsync-watchdog-heartbeat.txt")          # dev2 still runs the old single unit
    p = d.run(_HB)
    assert p.returncode == 0, p.stderr
    assert "snv watchdog: last_heartbeat_epoch=<none>" not in p.stderr
    assert d.notifies() == [], "SNV must not page stale while dev2 still writes the old path"


def test_snv_heartbeat_prefers_the_per_venue_file(tmp_path):
    d = Dev2(tmp_path)
    d.heartbeat("avsync-watchdog-heartbeat.txt", age=100000)     # the old unit's leftover, stale
    d.heartbeat("avsync-watchdog-heartbeat-snv.txt")             # the template instance, fresh
    p = d.run(_HB)
    assert p.returncode == 0, p.stderr
    assert d.notifies() == []


def test_pp_never_reads_the_old_single_file(tmp_path):
    d = Dev2(tmp_path)
    d.heartbeat("avsync-watchdog-heartbeat.txt")
    d.heartbeat("avsync-watchdog-heartbeat-snv.txt")
    p = d.run(_HB, home_boxes="stream-pp")
    assert p.returncode == 0, p.stderr
    calls = d.notifies()
    assert len(calls) == 1 and "[PP]" in calls[0], calls


def test_lineup_snv_reads_the_old_single_file_too(tmp_path):
    d = Dev2(tmp_path)
    d.heartbeat("avsync-watchdog-heartbeat.txt", status="measured: db=-91.0 [x] UNMEASURABLE window")
    p = d.run(_LINEUP, ["--dry-run"])
    assert p.returncode == 0, p.stderr
    assert "snv: stream_active=true" in p.stderr and "sig=no-audio" in p.stderr


def test_snv_report_reads_the_old_day_log_too(tmp_path):
    d = Dev2(tmp_path)
    d.heartbeat("avsync-watchdog-heartbeat-snv.txt")
    day = time.strftime("%Y-%m-%d")
    (d.home / "avsync" / f"measurements-{day}.tsv").write_text(
        f"{int(time.time())}\tmeasured: db=-5.0 [x] AV offset +0 fr (+0 ms) conf 6.0 :: A/V sync OK (offset 0 ms)\n"
    )
    p = d.run(_HB, ["--dry-run"], extra={"AVSYNC_REPORT_FETCH_CMD": ""})
    assert p.returncode == 0, p.stderr
    assert "WOULD post report message: [SNV] 📐 A/V overené" in p.stderr


# ── 3. a table that lost the legacy venue, or is empty, pages ────────────────────────────────────


def test_a_table_without_the_legacy_venue_pages(tmp_path):
    d = Dev2(tmp_path)
    p = d.run(_HB, extra={"AVSYNC_VENUE_TABLE": "pp|stream-pp|stream-pp.lan|stream-pp\n"})
    assert p.returncode == 0, p.stderr
    calls = [c for c in d.notifies() if "--dedup-key avsync-venue-table" in c]
    assert len(calls) == 1, d.notifies()
    assert "snv" in calls[0].lower()


def test_an_empty_table_pages(tmp_path):
    d = Dev2(tmp_path)
    empty = tmp_path / "noboxes"
    empty.mkdir()
    p = d.run(_HB, extra={"STRIH_BOXES_DIR": str(empty), "OBS_FLEET": "strih-snv|10.77.9.202|linux-genlock|always\n"})
    assert p.returncode == 0, p.stderr
    assert any("--dedup-key avsync-venue-table" in c for c in d.notifies()), d.notifies()


def test_an_explicit_venue_narrowing_never_pages_the_table(tmp_path):
    d = Dev2(tmp_path)
    d.heartbeat("avsync-watchdog-heartbeat-pp.txt")
    p = d.run(_HB, extra={"AVSYNC_VENUES": "pp"}, home_boxes="stream-pp")
    assert p.returncode == 0, p.stderr
    assert d.notifies() == []


def test_the_whole_table_present_never_pages_the_table(tmp_path):
    d = Dev2(tmp_path)
    d.heartbeat("avsync-watchdog-heartbeat-snv.txt")
    p = d.run(_HB)
    assert p.returncode == 0, p.stderr
    assert d.notifies() == []


# ── 4. the pre-issue-1441 report state moves to the legacy venue's file once ────────────────────


def test_the_old_report_state_moves_to_the_snv_file_without_a_repost(tmp_path):
    d = Dev2(tmp_path)
    d.heartbeat("avsync-watchdog-heartbeat-snv.txt")
    fetch = (
        'printf "%s\\t%s\\n" "$(date +%s)" '
        '"measured: db=-5.0 [x] AV offset +0 fr (+0 ms) conf 6.0 :: A/V sync OK (offset 0 ms)"'
    )
    extra = {"AVSYNC_REPORT_FETCH_CMD": fetch, "AVSYNC_VENUES": "snv"}
    p = d.run(_HB, extra=extra)
    assert p.returncode == 0, p.stderr
    assert len(d.curls()) == 1, "the first pass posts the START"
    snv = tmp_path / "report-state-snv.json"
    old = tmp_path / "report-state.json"
    snv.rename(old)                                  # the state as the pre-issue-1441 watchdog left it
    p = d.run(_HB, extra=extra)
    assert p.returncode == 0, p.stderr
    assert len(d.curls()) == 1, "the migrated state must not re-post the START"
    assert json.loads(snv.read_text())["start_posted"] is True
    assert not old.exists()


def test_dry_run_never_moves_the_old_report_state(tmp_path):
    d = Dev2(tmp_path)
    d.heartbeat("avsync-watchdog-heartbeat-snv.txt")
    old = tmp_path / "report-state.json"
    old.write_text(json.dumps({"start_posted": True}))
    fetch = 'printf "%s\\t%s\\n" "$(date +%s)" "measured: db=-5.0 [x] AV offset +0 fr (+0 ms) conf 6.0 :: x"'
    p = d.run(_HB, ["--dry-run"], extra={"AVSYNC_REPORT_FETCH_CMD": fetch, "AVSYNC_VENUES": "snv"})
    assert p.returncode == 0, p.stderr
    assert old.exists() and not (tmp_path / "report-state-snv.json").exists()


# ── 5. AVSYNC_VENUES is never glob-expanded ──────────────────────────────────────────────────────


def test_avsync_venues_is_never_glob_expanded(tmp_path):
    (tmp_path / "snv").write_text("")
    (tmp_path / "pp").write_text("")
    p = subprocess.run(
        ["bash", "-c", 'set -euo pipefail; . "$1"; avsync_selected_venues', "x", str(_VENUES)],
        env={"PATH": "/usr/bin:/bin", "AVSYNC_VENUES": "*"}, cwd=tmp_path,
        capture_output=True, text=True,
    )
    assert p.returncode == 0, p.stderr
    assert p.stdout == "", "a '*' must not expand to the files named snv / pp"
    assert "'*'" in p.stderr


# ── 6. the table is derived once per pass ────────────────────────────────────────────────────────


def test_the_table_warnings_print_once_per_pass(tmp_path):
    d = Dev2(tmp_path)
    d.heartbeat("avsync-watchdog-heartbeat-snv.txt")
    table = "snv|stream-snv|10.77.9.204|obs-e2e-test\nbad|-x|h|k\n"
    p = d.run(_HB, ["--dry-run"], extra={"AVSYNC_VENUE_TABLE": table})
    assert p.returncode == 0, p.stderr
    assert p.stderr.count("dropping malformed row") == 1, p.stderr


# ── review round 2 ───────────────────────────────────────────────────────────────────────────────


def test_an_inherited_cache_is_never_trusted():
    # only an in-process avsync_venue_table_load may fill the cache: variables inherited from the
    # environment are dropped at source time, so an injected unvalidated row never reaches a caller.
    key = subprocess.run(
        ["bash", "-c", '. "$1"; _avsync_venue_cache_key', "x", str(_VENUES)],
        env={"PATH": "/usr/bin:/bin"}, capture_output=True, text=True,
    ).stdout
    p = subprocess.run(
        ["bash", "-c", 'set -euo pipefail; . "$1"; avsync_venue_table', "x", str(_VENUES)],
        env={"PATH": "/usr/bin:/bin", "_AVSYNC_VENUE_ROWS_KEY": key,
             "_AVSYNC_VENUE_ROWS": "evil|b|h;rm|k"},
        capture_output=True, text=True,
    )
    assert p.returncode == 0, p.stderr
    assert "evil" not in p.stdout
    assert p.stdout.splitlines()[0] == "snv|stream-snv|10.77.9.204|obs-e2e-test"


def test_assert_is_no_go_when_the_table_lost_the_legacy_venue(tmp_path):
    d = Dev2(tmp_path)
    d.heartbeat("avsync-watchdog-heartbeat-pp.txt")
    systemctl = _exe(d.stub / "systemctl", "#!/bin/sh\nexit 0\n")
    p = d.run(
        _LINEUP, ["--assert"], home_boxes="stream-pp",
        extra={"AVSYNC_VENUE_TABLE": "pp|stream-pp|stream-pp.lan|stream-pp\n",
               "AVSYNC_LINEUP_SYSTEMCTL": str(systemctl)},
    )
    assert p.returncode == 1, p.stdout + p.stderr
    assert any(l.startswith("NO-GO") and "venue table" in l for l in p.stdout.splitlines()), p.stdout
