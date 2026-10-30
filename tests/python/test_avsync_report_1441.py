"""issue 1441 -- the A/V report posts only what helps (owner 28.10.2026: "nech to nebol spam ale len
podstatne hlasky ktore niecomu pomahaju").

A session posts at most its FIRST VERDICT, each CHANGE after it, and one END -- and only a
PRODUCTION session posts at all. Replayed over the real dev2 day logs of 26.-28.10.2026 (the PP
event of 28.10. and the SNV TEST-mode development streams), no-signal rows dropped (they never
enter a session).
"""

import os
import pathlib
import stat
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import avsync_report as ar  # noqa: E402

_FIX = _ROOT / "tests" / "fixtures" / "avsync_report_1441"
_WATCHDOG = _SCRIPTS / "avsync-heartbeat-alert-watchdog.sh"
BASE = 1_800_000_000


def _clip(offset_ms, conf=6.0):
    return "measured: db=-5.0 [2026-10-28 18:00:00] AV offset {:+d} fr ({:+d} ms) conf {:.1f} :: x".format(
        offset_ms // 40, offset_ms, conf
    )


def _unmeasurable():
    return "measured: db=-23.0 [2026-10-28 18:00:00] UNMEASURABLE window (best confidence 1.2 < 3.0)"


def _replay(rows, production, step=300):
    """The dev1 timer evaluates every 5 min, from the first row to an hour after the last."""
    st, posts = {}, []
    for now in range(rows[0][0], rows[-1][0] + 3600, step):
        msgs, st = ar.decide([r for r in rows if r[0] <= now], st, now, production=production)
        posts += msgs
    return posts


# ── the real 26.-28.10. rows ─────────────────────────────────────────────────


def test_the_pp_event_of_28_10_posts_its_verdict_the_fix_and_the_end():
    rows = ar._read_rows(str(_FIX / "pp-2026-10-26_28.tsv"))
    posts = _replay(rows, "yes")
    assert len(posts) <= 4, posts
    assert len(posts) == 3, posts
    assert "(vysielanie)" in posts[0] and "NESEDÍ o 40 ms" in posts[0]
    assert "verdikt sa zmenil → SEDÍ" in posts[1]
    assert posts[2].startswith("📐 A/V počas vysielania 17:29–19:22") and "→ SEDÍ" in posts[2]
    assert not any("za 20 min" in p and "overené" in p and "(vysielanie)" not in p for p in posts)


def test_the_snv_test_mode_streams_post_nothing():
    rows = ar._read_rows(str(_FIX / "snv-2026-10-26_28.tsv"))
    assert len(rows) > 200
    assert _replay(rows, "no") == []


def test_the_old_report_spammed_those_same_rows():
    # The pre-1441 cadence posted ~one line per 20 min of measured stream; counted here so the
    # fixture keeps proving the problem (the SNV rows hold sessions longer than one window).
    rows = ar._read_rows(str(_FIX / "snv-2026-10-26_28.tsv"))
    long_sessions = [s for s in ar._split_sessions([r for r in ar._parse_rows(rows) if r.measured])
                     if s[-1].epoch - s[0].epoch >= ar.WINDOW_S]
    assert len(long_sessions) >= 5


# ── production gate ──────────────────────────────────────────────────────────


def test_unknown_production_posts_like_a_production():
    rows = [(BASE + i * 90, _clip(0)) for i in range(5)]
    msgs, st = ar.decide(rows, {}, BASE + 400, production="unknown")
    assert len(msgs) == 1 and st["production"] is True


def test_a_garbled_production_value_reads_as_unknown():
    rows = [(BASE + i * 90, _clip(0)) for i in range(5)]
    msgs, _st = ar.decide(rows, {}, BASE + 400, production="maybe")
    assert len(msgs) == 1


def test_a_development_session_posts_not_even_its_end():
    rows = [(BASE + i * 90, _clip(120)) for i in range(20)]
    st = {}
    for now in range(BASE, BASE + 20 * 90 + 900, 300):
        msgs, st = ar.decide([r for r in rows if r[0] <= now], st, now, production="no")
        assert msgs == []
    assert st["end_posted"] is True


def test_production_latches_for_the_rest_of_the_session():
    # EVENT read once mid-session, then the probe reads TEST (cam2 restarted) -- the END still posts.
    rows = [(BASE + i * 90, _clip(0)) for i in range(20)]
    _m, st = ar.decide(rows[:5], {}, BASE + 400, production="yes")
    assert st["production"] is True
    msgs, st = ar.decide(rows, st, rows[-1][0] + 700, production="no")
    assert any(m.startswith("📐 A/V počas vysielania") for m in msgs)


def test_a_session_seen_only_after_it_ended_takes_this_passes_reading():
    rows = [(BASE + i * 90, _clip(0)) for i in range(20)]
    msgs, _st = ar.decide(rows, {}, rows[-1][0] + 700, production="yes")
    assert len(msgs) == 1 and msgs[0].startswith("📐 A/V počas vysielania")
    msgs, _st = ar.decide(rows, {}, rows[-1][0] + 700, production="no")
    assert msgs == []


# ── END worthiness ───────────────────────────────────────────────────────────


def test_a_short_blip_without_a_verdict_has_no_end():
    rows = [(BASE + i * 120, _unmeasurable()) for i in range(4)]
    st = {}
    for now in range(BASE, BASE + 1500, 300):
        msgs, st = ar.decide([r for r in rows if r[0] <= now], st, now, production="yes")
        assert msgs == []


def test_a_long_production_the_measurement_could_not_judge_gets_one_end():
    rows = [(BASE + i * 90, _unmeasurable()) for i in range(20)]  # 28.5 min, never a verdict
    posts = _replay(rows, "yes")
    assert len(posts) == 1 and "žiadne spoľahlivé meranie" in posts[0]


def test_a_flip_and_back_posts_each_change_once():
    rows = [(BASE + i * 90, _clip(0)) for i in range(14)]
    rows += [(BASE + 1260 + i * 90, _clip(120)) for i in range(14)]
    rows += [(BASE + 2520 + i * 90, _clip(0)) for i in range(14)]
    posts = _replay(rows, "yes")
    kinds = ["first" if "(vysielanie)" in p else "change" if "zmenil" in p else "end" for p in posts]
    assert kinds == ["first", "change", "change", "end"], posts


# ── the watchdog's production reading ────────────────────────────────────────


def _fake_sshpass(tmp_path, snapshot):
    binp = tmp_path / "bin"
    binp.mkdir()
    fake = binp / "sshpass"
    fake.write_text("#!/bin/sh\nprintf '%s\\n' " + " ".join("'%s'" % ln for ln in snapshot) + "\n")
    fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
    return binp


_EVENT = ["RIG_MODE_PROBE_OK", "PID_PRESENT|0", "PID_ALIVE|0", "SVC_ENABLED|0", "SVC_ACTIVE|0"]
_TEST = ["RIG_MODE_PROBE_OK", "PID_PRESENT|1", "PID_ALIVE|1", "SVC_ENABLED|1", "SVC_ACTIVE|1"]


def _production_line(tmp_path, venue, snapshot):
    binp = _fake_sshpass(tmp_path, snapshot)
    fetch = "printf '%s\\tmeasured: db=-23.0 [x] UNMEASURABLE window\\n' \"$(date +%s)\""
    env = {
        "PATH": f"{binp}:/usr/bin:/bin",
        "HOME": str(pathlib.Path.home()),
        "AVSYNC_HEARTBEAT_HOST": "none",
        "AVSYNC_VENUES": venue,
        "OBS_FLEET_HOME": "stream-pp",
        "AVSYNC_HEARTBEAT_STATE_FILE": str(tmp_path / "hb.state"),
        "AVSYNC_REPORT_STATE_FILE": str(tmp_path / "report-state.json"),
        "AVSYNC_REPORT_FETCH_CMD": fetch,
    }
    proc = subprocess.run(["bash", str(_WATCHDOG), "--dry-run"], env=env, capture_output=True, text=True)
    assert proc.returncode == 0, proc.stderr
    return [ln for ln in proc.stderr.splitlines() if "report: production=" in ln]


def test_snv_reads_event_mode_as_production(tmp_path):
    assert _production_line(tmp_path, "snv", _EVENT)[0].endswith("snv report: production=yes")


def test_snv_reads_test_mode_as_development(tmp_path):
    assert _production_line(tmp_path, "snv", _TEST)[0].endswith("snv report: production=no")


def test_snv_unreadable_rig_mode_reports_a_measured_session(tmp_path):
    # cam2 unreachable: the fetched rows already prove the stream ran, so the report posts.
    assert _production_line(tmp_path, "snv", [])[0].endswith("snv report: production=yes")


def test_pp_is_a_production_whenever_it_measures(tmp_path):
    # The rig probe would read TEST; PP never asks it.
    assert _production_line(tmp_path, "pp", _TEST)[0].endswith("pp report: production=yes")


def test_the_fixtures_are_the_real_rows_without_no_signal():
    for name in os.listdir(_FIX):
        text = (_FIX / name).read_text(encoding="utf-8")
        assert "\tno-signal" not in text
        assert all(line.split("\t", 1)[1].startswith("measured: ") for line in text.splitlines())
