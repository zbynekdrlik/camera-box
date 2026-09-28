"""issue 1381 -- replay the REAL resolume cg OBS log (27.9.2026) through the whole pager.

Fixtures (tests/fixtures/audio_mixer_1381/, gzip, CRLF as on the box): every `audio-stall #1367`
and `obs-vban` line of four windows, cut read-only from the resolume OBS logs with Select-String:

  resolume-control-0300-0415  -- 27.9 03:00-04:15 (log `2026-09-26 16-35-04.txt`): music on
                                 program, no dock; 2813 ticks/min, pacers flat at 1/0/1. SILENT.
  resolume-onset-0540-0630    -- 27.9 05:40-06:30 (same log): the pacers start losing audio at
                                 06:00:14, ticks_over 22 at 06:00 and 36 at 06:01. PAGES.
  resolume-start-1949-1957    -- 27.9 19:49-19:57 (log `2026-09-27 19-49-08.txt`): a normal OBS
                                 start (partial ticks=1 dump, counters=reset). SILENT.
  resolume-clean-2000-2045    -- the same log 20:00-20:45, after the 716 deploy. SILENT.

A pass at time T sees every line logged up to T (the box's :8899 gather reads the log as it stood),
exactly like the dev1 timer every 5 minutes. The python replay sweeps every pass phase with the
same 2-pass confirm as scripts/lib/obs-watchdog-decision.sh; one phase per fixture is also driven
through the REAL scripts/audio-mixer-alert-watchdog.sh --dry-run via its FETCH_CMD seam.
"""
import gzip
import json
import os
import pathlib
import stat
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_FX = _ROOT / "tests" / "fixtures" / "audio_mixer_1381"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import audio_mixer_decision as amd  # noqa: E402
import bundle_state_gather as bsg  # noqa: E402

PASS_S = 300
PAGING = {"mixer": ("BEHIND", "OVERLOADED"), "vban": ("VBAN_LOSS",)}


def _lines(name):
    raw = gzip.decompress((_FX / f"resolume-{name}.txt.gz").read_bytes()).decode("ascii")
    return raw.splitlines(keepends=True)


def _sec(line):
    h, m, s = line[:12].split(":")
    return int(h) * 3600 + int(m) * 60 + float(s)


def _bundle_at(lines, t):
    """The /bundle-state.json body the box would serve at in-log time t."""
    text = "".join(ln for ln in lines if _sec(ln) <= t)
    mixer = bsg.audio_mixer_from_log(text)
    vban = bsg.vban_pacer_loss_from_log(text)
    st = bsg.build_bundle_state(
        audio_mixer_ticks=mixer[0], audio_mixer_ticks_over=mixer[1],
        audio_mixer_window_ms=mixer[2], audio_mixer_tick_ms=mixer[3], audio_mixer_age_s=mixer[4],
        vban_pacer_loss_events=vban[0], vban_pacer_loss_ms=vban[1], vban_pacer_loss_dest=vban[2],
        vban_pacer_age_s=vban[3])
    return json.dumps(st)


def _replay(lines, phase_s):
    """-> {arm: [(t, verdict, confirm, act)]} over every pass of the window at this phase."""
    start, end = _sec(lines[0]), _sec(lines[-1])
    t = start + phase_s
    confirm = {"mixer": 0, "vban": 0}
    out = {"mixer": [], "vban": []}
    while t <= end:
        r = amd.analyze(_bundle_at(lines, t), 1)
        for arm, key in (("mixer", "mixer_verdict"), ("vban", "vban_verdict")):
            v = r[key]
            act = 0
            if v in PAGING[arm]:
                confirm[arm] += 1
                act = 1 if confirm[arm] >= 2 else 0
            elif v == "HEALTHY" or (arm == "mixer" and v == "STALE"):
                confirm[arm] = 0
            # SKIP / UNKNOWN (and the VBAN arm's STALE) hold the counter, exactly as the watchdog
            # does; the mixer arm's STALE resets it (issue 1385: a log that is not live).
            out[arm].append((t, v, confirm[arm], act))
        t += PASS_S
    return out


def _first_page(passes):
    for t, _v, _c, act in passes:
        if act:
            return t
    return None


def _hms(sec):
    sec = int(sec) % 86400
    return f"{sec // 3600:02d}:{(sec % 3600) // 60:02d}:{sec % 60:02d}"


@pytest.mark.parametrize("name", ["control-0300-0415", "start-1949-1957", "clean-2000-2045"])
def test_quiet_windows_never_page_at_any_pass_phase(name):
    lines = _lines(name)
    for phase in range(0, PASS_S, 20):
        res = _replay(lines, phase)
        for arm in ("mixer", "vban"):
            fired = [(_hms(t), v) for t, v, _c, act in res[arm] if v in PAGING[arm]]
            assert not fired, f"{name} phase {phase}s {arm} graded a paging verdict: {fired}"


def test_normal_start_reads_unknown_until_the_second_dump():
    lines = _lines("start-1949-1957")
    first_stall = next(_sec(ln) for ln in lines if "audio-stall #1367" in ln)
    r = amd.analyze(_bundle_at(lines, first_stall + 30), 1)
    assert r["mixer_verdict"] == "UNKNOWN"
    r = amd.analyze(_bundle_at(lines, first_stall + 90), 1)
    assert r["mixer_verdict"] == "HEALTHY"


def test_onset_pages_the_vban_arm_by_the_second_pass_after_0600():
    lines = _lines("onset-0540-0630")
    onset = 6 * 3600 + 14    # 06:00:14, the first moved pacer counter
    for phase in range(0, PASS_S, 20):
        res = _replay(lines, phase)
        t = _first_page(res["vban"])
        assert t is not None, f"phase {phase}s: the VBAN arm never paged"
        assert onset < t <= onset + 2 * PASS_S + 1, f"phase {phase}s: paged at {_hms(t)}"
        before = [(_hms(x), v) for x, v, _c, _a in res["vban"] if x < onset and v != "HEALTHY"]
        assert not before, f"phase {phase}s: non-HEALTHY before the onset: {before}"


def test_onset_pages_the_mixer_arm_within_the_first_quarter_hour():
    # Minute by minute the onset alternates (06:00 22, 06:01 36, 06:02 12, 06:03 43, 06:04 10 late
    # ticks, then 215+ and BEHIND from 06:09), so a 5-min pass that lands on the quiet minutes
    # confirms only on 06:09 + 06:14 -- the worst phase pages at ~06:15:15.
    lines = _lines("onset-0540-0630")
    onset = 6 * 3600 + 60 + 15    # 06:01:15, ticks_over=36
    for phase in range(0, PASS_S, 20):
        res = _replay(lines, phase)
        t = _first_page(res["mixer"])
        assert t is not None, f"phase {phase}s: the mixer arm never paged"
        assert t <= 6 * 3600 + 16 * 60, f"phase {phase}s: mixer paged only at {_hms(t)}"
        before = [(_hms(x), v) for x, v, _c, _a in res["mixer"]
                  if x < onset - 60 and v in PAGING["mixer"]]
        assert not before, f"phase {phase}s: mixer graded a page before 06:00: {before}"


# ---------------------------------------------------------------------------------------------
# the real bash orchestrator over the same replay (its FETCH_CMD seam, --dry-run, scratch state)
# ---------------------------------------------------------------------------------------------
def _run_watchdog(tmp_path, body, state_file):
    body_file = tmp_path / "body.json"
    body_file.write_text(body, encoding="utf-8")
    fetch = tmp_path / "fetch.sh"
    fetch.write_text(f"#!/usr/bin/env bash\ncat '{body_file}'\n", encoding="utf-8")
    fetch.chmod(fetch.stat().st_mode | stat.S_IEXEC)
    env = dict(os.environ)
    env.update({
        "AUDIO_MIXER_FETCH_CMD": str(fetch),
        "AUDIO_MIXER_BOXES": "resolume|resolume.lan",
        "AUDIO_MIXER_ALERT_STATE_FILE": str(state_file),
        "OBS_FLEET_HOME": "resolume",
        "AIRULESET_NOTIFY": "/nonexistent/airuleset.py",
    })
    r = subprocess.run(["bash", str(_SCRIPTS / "audio-mixer-alert-watchdog.sh"), "--dry-run"],
                       capture_output=True, text=True, env=env, timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stderr


@pytest.mark.parametrize("name,expect_page", [
    ("onset-0540-0630", True),
    ("control-0300-0415", False),
    ("clean-2000-2045", False),
    ("start-1949-1957", False),
])
def test_watchdog_dry_run_replay(tmp_path, name, expect_page):
    lines = _lines(name)
    state = tmp_path / "state"
    start, end = _sec(lines[0]), _sec(lines[-1])
    t = start + 60
    would = []
    while t <= end:
        log = _run_watchdog(tmp_path, _bundle_at(lines, t), state)
        would += [(_hms(t), ln) for ln in log.splitlines() if "WOULD alert" in ln]
        t += PASS_S
    if expect_page:
        assert any("VBAN_LOSS" in ln for _t, ln in would), would
        assert any(("OVERLOADED" in ln or "BEHIND" in ln) for _t, ln in would), would
        assert all(tt >= "06:00:00" for tt, _ln in would), would
    else:
        assert not would, would
