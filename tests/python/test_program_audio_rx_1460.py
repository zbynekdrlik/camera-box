"""issue 1460 -- the program-audio RECEIVE loss pager (validation 10.11.2026, design 6757370365).

The program audio of a Linux OBS box comes in through ONE receiver, named by the box facts: the
obs-vban source (stream-pp, strih-pp: `STRIH_PROGRAM_VBAN_STREAMS != none`) or the Dante input
(strih-snv, stream-snv: a declared `<role>-obs` device). Both log their loss counters every 10 s; only
verify item 9 and the E2E window gates read them. This slice:

  * gather `bundle_state_gather.audio_rx_from_log`: the nested `audio_rx` facet, per receiver the
    gated counter deltas of the last 660 s (both status lines inside the window), a re-init / a counter
    that went down = a REBASE, never a loss;
  * decision `audio_mixer_decision.classify_program_rx`;
  * `scripts/lib/program-audio-rx.sh` `program_audio_rx_key`: the receiver from the facts;
  * the fifth arm of the real bash audio-mixer watchdog through `AUDIO_MIXER_FETCH_CMD`.

Tier-0 pytest (no cargo).
"""
import importlib.util
import json
import os
import pathlib
import re
import stat
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import audio_mixer_decision as amd  # noqa: E402
import bundle_state_gather as bsg  # noqa: E402
import dante_input_continuity as dic  # noqa: E402

VBAN_C = _ROOT / "vendor/obs-vban/src/vban-source.c"
REAL_LOG = _ROOT / "tests/fixtures/program_audio_continuity_1439/stream-snv-2026-10-23-from-41400.txt"


def _c_format(path, head):
    """The C format string starting at `head`, its literals joined, PRIu64 / %ld / %.*s made python
    (the program_audio_continuity_1439 renderer: never a hand-typed copy of the line)."""
    src = re.sub(r'"\s*PRI[ud]64\s*"', "d", path.read_text())
    src = re.sub(r'"\s*PRI[ud]64\s*,', 'd",', src)
    start = src.index(head)
    end = src.index('",', start) + 1
    fmt = "".join(re.findall(r'"((?:[^"\\]|\\.)*)"', src[start:end]))
    return fmt.replace("%ld", "%d").replace("%.*s", "%s")


def _vban(ts, lost=0, received=1000, re_anchors=0, missed=0, late=0, concealed=0, stale=0,
          name="Zvuk z mixu"):
    fmt = _c_format(VBAN_C, '"obs-vban receive: received=%')
    body = fmt % (received, lost, late, re_anchors, stale, 0, concealed, 0.0, -30.0, 0.5, 0.2,
                  "locked", -28.0, missed, 40, 48000, 2, 6980, "stream", name)
    return f"{ts}: [obs-vban] {body}"


def _dante(ts, **k):
    v = dict(clock="media", holdover="0", gm="001dc1fffe0a", polls="1", re_anchors="0", rx_packets="10",
             rx_loss="0", rx_gaps="0", rx_late="0", socket_drops="0", blocks="5", underruns="0",
             overruns="0", delivered_frames="100", rx_holes="0", rx_written_late="0", late_blocks="0",
             rebootstraps="0", max_process_us="80", obs_dropped="0", max_obs_us="40")
    v.update({key: str(val) for key, val in k.items()})
    return "%s: dante-input status: %s" % (ts, " ".join("%s=%s" % i for i in v.items()))


def _t(s):
    """seconds after 12:00:00 -> an OBS log stamp."""
    return "12:%02d:%02d.000" % divmod(s, 60)


def _text(lines):
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------------------------
# gather: audio_rx_from_log
# ---------------------------------------------------------------------------------------------
def test_the_rendered_vban_line_matches_the_live_shape():
    line = _vban(_t(0), lost=3)
    assert "obs-vban receive: received=1000 lost=3 late=0 re_anchors=0 stale=0" in line
    assert line.endswith("port=6980 stream='stream' source='Zvuk z mixu'"), line


def test_no_receiver_line_is_absent():
    assert bsg.audio_rx_from_log("") is None
    assert bsg.audio_rx_from_log(_text([f"{_t(0)}: program-render-audit: x"])) is None


def test_a_clean_receiver_reads_zero_events_and_its_age():
    log = _text([_vban(_t(s), received=1000 + s) for s in range(0, 61, 10)] + [f"{_t(64)}: x"])
    rx = bsg.audio_rx_from_log(log)
    e = rx["obs-vban:Zvuk z mixu"]
    assert (e["kind"], e["name"], e["events"], e["pairs"], e["rebases"], e["age_s"]) == \
        ("obs-vban", "Zvuk z mixu", 0, 6, 0, 4)
    assert e["deltas"]["lost"] == 0 and e["gap_max_s"] == 10


def test_lost_re_anchors_and_emit_missed_are_the_events_and_late_is_named_only():
    log = _text([_vban(_t(0)), _vban(_t(10), lost=4, concealed=3, late=2),
                 _vban(_t(20), lost=4, concealed=3, late=2, re_anchors=1, missed=2)])
    e = bsg.audio_rx_from_log(log)["obs-vban:Zvuk z mixu"]
    assert e["events"] == 4 + 1 + 2
    assert e["deltas"]["late"] == 2 and e["deltas"]["concealed"] == 3


def test_a_concealed_only_loss_is_still_an_event():
    log = _text([_vban(_t(0)), _vban(_t(10), lost=2, concealed=2)])
    assert bsg.audio_rx_from_log(log)["obs-vban:Zvuk z mixu"]["events"] == 2


def test_only_pairs_with_both_lines_inside_the_window_count():
    # a loss between 11:40 and 11:50, both lines older than the window behind the head at 12:00:00
    lines = [f"11:40:00.000: [obs-vban] " + _vban("x")[len("x: [obs-vban] "):],
             "11:48:00.000: [obs-vban] " + _vban("x", lost=9)[len("x: [obs-vban] "):],
             "11:50:00.000: [obs-vban] " + _vban("x", lost=9)[len("x: [obs-vban] "):],
             _vban(_t(0), lost=9)]
    e = bsg.audio_rx_from_log(_text(lines))["obs-vban:Zvuk z mixu"]
    assert e["events"] == 0 and e["pairs"] == 1, e


def test_a_sender_pause_longer_than_the_window_never_reads_as_one_loss():
    # the console off: the status line stops (it is written per received packet); at resume the
    # starve second is counted lost + one re-anchor, against a line far outside the window
    lines = ["10:00:00.000: [obs-vban] " + _vban("x", lost=5)[len("x: [obs-vban] "):],
             _vban(_t(0), lost=192, re_anchors=1), _vban(_t(10), lost=192, re_anchors=1)]
    e = bsg.audio_rx_from_log(_text(lines))["obs-vban:Zvuk z mixu"]
    assert e["events"] == 0 and e["pairs"] == 1


def test_a_short_sender_pause_inside_the_window_is_a_loss_and_shows_its_gap():
    lines = [_vban(_t(0), lost=5), _vban(_t(130), lost=197, re_anchors=1)]
    e = bsg.audio_rx_from_log(_text(lines))["obs-vban:Zvuk z mixu"]
    assert e["events"] == 192 + 1 and e["gap_max_s"] == 130


def test_a_counter_that_went_down_is_a_rebase_never_a_loss():
    lines = [_vban(_t(0), lost=40, received=90000), _vban(_t(10), lost=0, received=300),
             _vban(_t(20), lost=1, received=700)]
    e = bsg.audio_rx_from_log(_text(lines))["obs-vban:Zvuk z mixu"]
    assert e["rebases"] == 1 and e["events"] == 1, e


@pytest.mark.parametrize("reinit", [
    "[obs-vban] obs-vban receive anchored: source 'Zvuk z mixu' frame=12 rate=48000 ch=2 "
    "receive_buffer_ms=40 -- holding its first second",
    "[obs-vban] obs-vban receive worker: source 'Zvuk z mixu' stopped (update)",
])
def test_a_reinit_line_makes_the_next_pair_a_rebase_even_when_the_counts_grew(reinit):
    # a young re-created source whose counters already passed the old ones: no backwards step
    lines = [_vban(_t(0), lost=1, received=100), f"{_t(5)}: {reinit}",
             _vban(_t(10), lost=3, received=400), _vban(_t(20), lost=3, received=900)]
    e = bsg.audio_rx_from_log(_text(lines))["obs-vban:Zvuk z mixu"]
    assert e["rebases"] == 1 and e["events"] == 0, e


def test_a_reinit_of_another_source_does_not_rebase_this_one():
    lines = [_vban(_t(0)), f"{_t(5)}: [obs-vban] obs-vban receive worker: source 'VBAN bench' stopped (destroy)",
             _vban(_t(10), lost=2)]
    assert bsg.audio_rx_from_log(_text(lines))["obs-vban:Zvuk z mixu"]["events"] == 2


def test_two_sources_are_kept_apart():
    lines = [_vban(_t(0)), _vban(_t(1), name="VBAN bench (issue 1460)"),
             _vban(_t(10)), _vban(_t(11), lost=6, name="VBAN bench (issue 1460)")]
    rx = bsg.audio_rx_from_log(_text(lines))
    assert rx["obs-vban:Zvuk z mixu"]["events"] == 0
    assert rx["obs-vban:VBAN bench (issue 1460)"]["events"] == 6


def test_the_dante_gated_tuple_is_the_e2e_gates():
    assert bsg.DANTE_RX_GATED == dic.GATED


def test_a_dante_loss_is_an_event_and_rx_late_is_named_only():
    lines = [_dante(_t(0)), _dante(_t(10), rx_loss=2, rx_late=5), _dante(_t(20), rx_loss=2, rx_late=5,
                                                                      rx_holes=1, underruns=1)]
    e = bsg.audio_rx_from_log(_text(lines))[bsg.DANTE_RX_KEY]
    assert (e["kind"], e["name"], e["events"]) == ("dante", "dante-input", 4)
    assert e["deltas"]["rx_late"] == 5


def test_a_dante_start_line_rebases():
    lines = [_dante(_t(0), rx_loss=3), f"{_t(5)}: dante-input start: config=x name=strih-obs",
             _dante(_t(10), rx_loss=4), _dante(_t(20), rx_loss=4)]
    e = bsg.audio_rx_from_log(_text(lines))[bsg.DANTE_RX_KEY]
    assert e["rebases"] == 1 and e["events"] == 0


def test_a_dante_line_of_an_older_release_counts_what_it_carries():
    old = [_dante(_t(0)).replace(" rx_holes=0", ""), _dante(_t(10), rx_loss=1).replace(" rx_holes=0", "")]
    e = bsg.audio_rx_from_log(_text(old))[bsg.DANTE_RX_KEY]
    assert e["events"] == 1 and "rx_holes" not in e["deltas"]


def test_a_bounded_read_takes_only_the_tail():
    head = _text([_vban("11:00:00.000"), _vban("11:00:10.000", lost=5)])
    tail = _text([_vban(_t(0), lost=5), _vban(_t(10), lost=5)])
    e = bsg.audio_rx_from_log(head + bsg.LOG_BOUNDED_READ_SEPARATOR + tail)["obs-vban:Zvuk z mixu"]
    assert e["events"] == 0 and e["pairs"] == 1


def test_the_real_stream_snv_log_reads_the_e2e_gates_seven_lost_slots():
    # the program_audio_continuity_1439 fixture: lost 231 at 09:01:07 -> 238 at 09:02:17 (3 concealed)
    lines = REAL_LOG.read_text().splitlines()
    end = next(i for i, ln in enumerate(lines) if ln.startswith("09:02:17") and "obs-vban receive:" in ln)
    rx = bsg.audio_rx_from_log(_text(lines[:end + 1]), window_s=71)
    e = rx["obs-vban:mbc"]
    assert e["deltas"]["lost"] == 7 and e["deltas"]["concealed"] == 3, e
    assert e["events"] >= 7 and e["age_s"] == 0


def test_the_shared_tail_gives_the_same_result():
    log = _text([_vban(_t(0)), _vban(_t(10), lost=1)])
    assert bsg.audio_rx_from_log(log, tail=bsg.timestamped_tail_lines(log)) == bsg.audio_rx_from_log(log)


def _load_server():
    spec = importlib.util.spec_from_file_location("bundle_state_server_rx_1460",
                                                  _SCRIPTS / "bundle-state-server.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_the_server_returns_audio_rx_as_a_nested_facet():
    bss = _load_server()
    log = _text([_vban(_t(0)), _vban(_t(10), lost=2)])
    facets, nested = bss._parse_log_facets(log, 12 * 3600 + 12, "mbc")
    assert nested["audio_rx"]["obs-vban:Zvuk z mixu"]["events"] == 2
    assert "audio_rx" not in facets


# ---------------------------------------------------------------------------------------------
# decision: classify_program_rx
# ---------------------------------------------------------------------------------------------
_E = {"events": 0, "age_s": 3}


@pytest.mark.parametrize("entry, key, reachable, head, verdict", [
    (_E, "dante", 0, 1, "SKIP"),
    (None, "", 1, 1, "NONE"),
    (None, "dante", 1, 1, "UNKNOWN"),
    ({"age_s": 3}, "dante", 1, 1, "UNKNOWN"),
    ({"events": 2, "age_s": 3}, "dante", 1, amd.DEFAULT_LOG_FROZEN_S + 1, "STALE"),
    ({"events": 2, "age_s": amd.DEFAULT_STALE_AFTER_S + 50}, "dante", 1, 1, "LOSS"),
    ({"events": 0, "age_s": amd.DEFAULT_STALE_AFTER_S + 1}, "dante", 1, None, "STALE"),
    ({"events": 0, "age_s": amd.DEFAULT_STALE_AFTER_S}, "dante", 1, 1, "HEALTHY"),
    ({"events": 1, "age_s": 3}, "obs-vban:x", 1, None, "LOSS"),
    (_E, "obs-vban:x", 1, None, "HEALTHY"),
])
def test_the_program_rx_truth_table(entry, key, reachable, head, verdict):
    assert amd.classify_program_rx(entry, key, reachable, head) == verdict


def test_analyze_picks_the_facts_receiver_and_prints_it():
    body = json.dumps({"obs_log_head_age_s": "2", "audio_rx": {
        "obs-vban:Zvuk z mixu": {"kind": "obs-vban", "name": "Zvuk z mixu", "events": 3,
                                 "deltas": {"lost": 3, "concealed": 1, "late": 0}, "pairs": 5,
                                 "rebases": 0, "gap_max_s": 10, "age_s": 4},
        "obs-vban:VBAN bench": {"events": 99, "age_s": 1}}})
    out = subprocess.run([sys.executable, str(_SCRIPTS / "audio_mixer_decision.py"), "analyze",
                          "--box-reachable", "1", "--rx-key", "obs-vban:Zvuk z mixu"],
                         input=body, capture_output=True, text=True, check=True).stdout.splitlines()
    for line in ("rx_verdict=LOSS", "rx_key=obs-vban:Zvuk z mixu", "rx_events=3",
                 "rx_deltas=concealed=1,lost=3", "rx_rebases=0", "rx_gap_max_s=10", "rx_age_s=4"):
        assert line in out, out
    assert amd.analyze(body, 1)["rx_verdict"] == "NONE"
    assert amd.analyze(body, 1, rx_key="dante")["rx_verdict"] == "UNKNOWN"


# ---------------------------------------------------------------------------------------------
# scripts/lib/program-audio-rx.sh: the receiver from the facts, never a literal box list
# ---------------------------------------------------------------------------------------------
def _rx_key(name):
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "$1"; program_audio_rx_key "$2"; echo "rc=$?"',
                        "harness", str(_SCRIPTS / "lib/program-audio-rx.sh"), name],
                       capture_output=True, text=True, timeout=60)
    return r.stdout.strip().splitlines()


@pytest.mark.parametrize("box, want", [
    ("stream-pp", ["obs-vban:Zvuk z mixu", "rc=0"]),
    ("strih-pp", ["obs-vban:ASIO zvuk", "rc=0"]),
    ("strih-snv", ["dante", "rc=0"]),
    ("stream-snv", ["dante", "rc=0"]),
    ("resolume-snv", ["rc=0"]),
])
def test_the_receiver_comes_from_the_box_facts(box, want):
    assert _rx_key(box) == want


def test_the_vban_boxes_are_exactly_the_facts_with_program_vban_streams():
    vban = set()
    for env in sorted((_ROOT / "scripts/strih-boxes").glob("*.env")):
        m = re.search(r"^STRIH_PROGRAM_VBAN_STREAMS=(.*)$", env.read_text(), re.M)
        if m and m.group(1).strip() != "none":
            vban.add(env.stem)
    assert vban == {b for b in ("stream-pp", "strih-pp", "strih-snv", "stream-snv")
                    if _rx_key(b)[0].startswith("obs-vban:")}


# ---------------------------------------------------------------------------------------------
# the real bash watchdog through the AUDIO_MIXER_FETCH_CMD seam
# ---------------------------------------------------------------------------------------------
def _body(events, age=4, head=2, key="obs-vban:Zvuk z mixu", gap=10):
    return json.dumps({"obs_log_head_age_s": str(head), "audio_rx": {key: {
        "kind": "obs-vban", "name": key.split(":", 1)[-1], "events": events,
        "deltas": {"lost": events, "concealed": 0, "late": 0}, "pairs": 60, "rebases": 0,
        "gap_max_s": gap, "age_s": age}}})


def _run(tmp_path, body, state, box="stream-pp|stream-pp.lan", dry_run=True, notify=None, page="1"):
    body_file = tmp_path / "body.json"
    body_file.write_text(body, encoding="utf-8")
    fetch = tmp_path / "fetch.sh"
    fetch.write_text(f"#!/usr/bin/env bash\ncat '{body_file}'\n", encoding="utf-8")
    fetch.chmod(fetch.stat().st_mode | stat.S_IEXEC)
    env = dict(os.environ)
    env.update({"AUDIO_MIXER_FETCH_CMD": str(fetch), "AUDIO_MIXER_BOXES": box,
                "OBS_FLEET_HOME": "stream-pp strih-pp resolume-snv",
                "AUDIO_MIXER_ALERT_STATE_FILE": str(state),
                "AIRULESET_NOTIFY": notify or "/nonexistent/airuleset.py"})
    env.pop("AUDIO_MIXER_PROGRAM_RX_PAGE", None)
    if page is not None:
        env["AUDIO_MIXER_PROGRAM_RX_PAGE"] = page
    args = ["bash", str(_SCRIPTS / "audio-mixer-alert-watchdog.sh")] + (["--dry-run"] if dry_run else [])
    r = subprocess.run(args, capture_output=True, text=True, env=env, timeout=120)
    assert r.returncode == 0, r.stderr
    return r.stderr


def _pages(log):
    return [ln for ln in log.splitlines() if "WOULD alert (LOSS)" in ln]


def _alerts_now(log):
    return [ln for ln in _pages(log) if "alert_now=1" in ln]


def test_watchdog_a_loss_pages_on_its_first_pass_once_per_incident_and_recovers_log_only(tmp_path):
    # review round 1: a counter delta is a fact, never a noisy reading -- the timer's 300-420 s pass
    # gap let one pass alone see a loss in ~27 % of cases, so a 2-pass confirm missed real losses
    state = tmp_path / "state"
    first = _run(tmp_path, _body(3), state)
    page = _alerts_now(first)
    assert "program_rx=LOSS key=obs-vban:Zvuk z mixu" in first and len(page) == 1, first
    assert "VBAN vstup OBS 'Zvuk z mixu'" in page[0] and "lost=3" in page[0], page[0]
    second = _run(tmp_path, _body(3), state)          # the same loss, still in the window
    assert not _alerts_now(second), second
    healthy = _run(tmp_path, _body(0), state)
    assert "program_rx=HEALTHY" in healthy and not _pages(healthy), healthy
    assert [ln for ln in healthy.splitlines() if "RECOVERY:" in ln and "program-rx" in ln], healthy


def test_watchdog_a_long_gap_is_named_in_the_page(tmp_path):
    page = _alerts_now(_run(tmp_path, _body(190, gap=130), tmp_path / "state"))
    assert page and "medzera 130 s" in page[0], page


def test_watchdog_stale_never_pages(tmp_path):
    state = tmp_path / "state"
    for _ in range(3):
        log = _run(tmp_path, _body(0, age=amd.DEFAULT_STALE_AFTER_S + 1, head=amd.DEFAULT_LOG_LIVE_S + 1),
                   state)
        assert "program_rx=STALE" in log and not _pages(log), log


def test_watchdog_a_silent_program_on_a_live_log_is_named_and_logged_never_paged(tmp_path):
    # review round 1: the program stopped while OBS keeps logging (the sender, the NIC or the leg).
    # Named SILENT on the machine channel; paging it outside a production is an owner decision.
    state = tmp_path / "state"
    for _ in range(2):
        log = _run(tmp_path, _body(0, age=amd.DEFAULT_STALE_AFTER_S + 1, head=2), state)
        assert "program_rx=SILENT" in log and "SILENT" in log and not _pages(log), log


def test_watchdog_a_frozen_log_never_pages_its_last_loss(tmp_path):
    state = tmp_path / "state"
    for _ in range(3):
        log = _run(tmp_path, _body(3, head=amd.DEFAULT_LOG_FROZEN_S + 1), state)
        assert "program_rx=STALE" in log and not _pages(log), log


def test_watchdog_a_box_without_a_receiver_in_its_facts_is_none(tmp_path):
    state = tmp_path / "state"
    for _ in range(2):
        log = _run(tmp_path, _body(3), state, box="resolume-snv|resolume.lan")
        assert "program_rx=NONE" in log and not _pages(log), log


def test_watchdog_the_dante_box_grades_the_dante_entry(tmp_path):
    state = tmp_path / "state"
    body = _body(2, key="dante")
    page = _alerts_now(_run(tmp_path, body, state, box="strih-snv|10.77.9.202"))
    assert page and "Dante vstup OBS" in page[0], page


def test_watchdog_another_receivers_loss_never_pages_the_program(tmp_path):
    state = tmp_path / "state"
    body = _body(9, key="obs-vban:VBAN bench (issue 1460)")
    for _ in range(2):
        log = _run(tmp_path, body, state)
        assert "program_rx=UNKNOWN" in log and not _pages(log), log


def test_watchdog_sends_a_time_bucketed_key(tmp_path):
    calls = tmp_path / "notify-calls.txt"
    notify = tmp_path / "notify.py"
    notify.write_text("import sys\n"
                      f"open({str(calls)!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n",
                      encoding="utf-8")
    state = tmp_path / "state"
    for _ in range(2):
        _run(tmp_path, _body(4), state, dry_run=False, notify=str(notify))
    sent = calls.read_text(encoding="utf-8").splitlines()
    assert len(sent) == 1, sent
    assert re.search(r"--dedup-key program-rx-stream-pp-\d+$", sent[0]), sent[0]
    assert "✅" not in sent[0]


def test_watchdog_the_arm_ships_disabled_it_logs_a_loss_but_never_pages(tmp_path):
    calls = tmp_path / "notify-calls.txt"
    notify = tmp_path / "notify.py"
    notify.write_text("import sys\n"
                      f"open({str(calls)!r}, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n",
                      encoding="utf-8")
    state = tmp_path / "state"
    for _ in range(3):
        log = _run(tmp_path, _body(4), state, dry_run=False, notify=str(notify), page=None)
        assert "program_rx=LOSS" in log and "paging is DISABLED" in log, log
    assert not calls.exists()


def test_the_window_outlives_the_longest_pass_gap_of_the_timer():
    # review round 1: one pass must always see a loss pair (two status lines 10 s apart): the window
    # covers the longest gap between two passes (OnUnitActiveSec + RandomizedDelaySec) + one period
    timer = (_ROOT / "systemd/audio-mixer-alert-watchdog.timer").read_text()
    period = int(re.search(r"^OnUnitActiveSec=(\d+)min", timer, re.M).group(1)) * 60
    spread = int(re.search(r"^RandomizedDelaySec=(\d+)min", timer, re.M).group(1)) * 60
    assert bsg.AUDIO_RX_WINDOW_S >= period + spread + 10 + 60, (period, spread)


def test_the_pager_gates_the_e2e_obs_vban_counters_except_late_and_stale():
    # late: the late packet's slot is already counted in lost; stale: a duplicate or an already-out
    # slot (the slot is in lost), never a loss of its own -- a deliberate difference, pinned
    import program_audio_continuity as pac
    assert set(pac.VBAN_GATED) - set(bsg.VBAN_RX_GATED) == {"late", "stale"}
    assert set(bsg.VBAN_RX_GATED) <= set(pac.VBAN_GATED)


@pytest.mark.parametrize("bad", [dict(clock="free"), dict(holdover="1")])
def test_a_dante_clock_fault_in_the_window_is_an_event(bad):
    lines = [_dante(_t(0), delivered_frames=100), _dante(_t(10), delivered_frames=580, **bad),
             _dante(_t(20), delivered_frames=1060)]
    e = bsg.audio_rx_from_log(_text(lines))[bsg.DANTE_RX_KEY]
    assert e["deltas"]["clock_faults"] == 1 and e["events"] == 1, e


def test_a_dante_error_line_in_the_window_is_an_event():
    lines = [_dante(_t(0), delivered_frames=100), f"{_t(5)}: dante-input error: rx socket closed",
             _dante(_t(10), delivered_frames=580)]
    e = bsg.audio_rx_from_log(_text(lines))[bsg.DANTE_RX_KEY]
    assert e["deltas"]["errors"] == 1 and e["events"] == 1, e


def test_dante_frames_that_do_not_advance_are_silent_pairs_never_an_event():
    lines = [_dante(_t(s), delivered_frames=100) for s in (0, 10, 20)]
    e = bsg.audio_rx_from_log(_text(lines))[bsg.DANTE_RX_KEY]
    assert e["silent_pairs"] == 2 and e["pairs"] == 2 and e["events"] == 0, e
    moving = [_dante(_t(0), delivered_frames=100), _dante(_t(10), delivered_frames=580)]
    assert bsg.audio_rx_from_log(_text(moving))[bsg.DANTE_RX_KEY]["silent_pairs"] == 0


@pytest.mark.parametrize("entry, head, verdict", [
    ({"events": 0, "age_s": amd.DEFAULT_STALE_AFTER_S + 1}, 2, "SILENT"),
    ({"events": 0, "age_s": amd.DEFAULT_STALE_AFTER_S + 1}, amd.DEFAULT_LOG_LIVE_S + 1, "STALE"),
    ({"events": 0, "age_s": amd.DEFAULT_STALE_AFTER_S + 1}, None, "STALE"),
    ({"events": 0, "age_s": 3, "pairs": 5, "silent_pairs": 5}, 2, "SILENT"),
    ({"events": 0, "age_s": 3, "pairs": 5, "silent_pairs": 4}, 2, "HEALTHY"),
    ({"events": 2, "age_s": 3, "pairs": 5, "silent_pairs": 5}, 2, "LOSS"),
])
def test_silent_is_a_live_log_whose_program_stopped(entry, head, verdict):
    assert amd.classify_program_rx(entry, "dante", 1, head) == verdict
