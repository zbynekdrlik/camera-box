"""issue 1406 -- the PURE decision core of the dev1 obs-handles watchdog
(scripts/obs_handles_decision.py): one pass's `obs_handles` reading against the reference sample
the watchdog kept from its previous pass.

The calibration this pins (the 5.10.2026 readings on the ticket):
  * healthy stream OBS after a restart: ~5,790 handles, flat (+-16 over 2.5 min);
  * the Audio Monitor leak: +46.875 handles/s = 168,750/h, 4,066,772 after ~22 h;
  * the Windows per-process cap: 16,777,216 (2**24).
Bounds: GROWING at >= 5,000 handles/h over one pass interval (34x below the leak rate, ~300x above
the healthy wobble per hour); CEILING at 500,000 handles (86x healthy, 3% of the cap) or 80% of a
Linux box's own soft open-files limit when that is lower.
"""
from __future__ import annotations

import importlib.util
import json
import pathlib
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_MOD_PATH = _ROOT / "scripts" / "obs_handles_decision.py"

_spec = importlib.util.spec_from_file_location("obs_handles_decision", _MOD_PATH)
dec = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(dec)

LEAK_PER_S = 48000 / 1024   # one registry key per OBS audio tick
STREAM_START = 1_791_110_100  # 4.10.2026 12:35 local, the leaking obs64 pid 9224


def _body(handles, pid="9224", start=str(STREAM_START), limit=""):
    facet = {"obs_version": "32.2.0", "obs_process_count": "1",
             "obs_handles": str(handles), "obs_handles_pid": pid, "obs_handles_start": start}
    if limit:
        facet["obs_handles_limit"] = limit
    return json.dumps(facet)


def _run(body, now, ref=(None, None, None), reachable=1, **kw):
    ref_ident, ref_handles, ref_epoch = ref
    return dec.analyze(body, reachable, now, ref_ident, ref_handles, ref_epoch, **kw)


def test_the_calibrated_defaults_have_wide_margins_both_ways():
    leak_per_h = LEAK_PER_S * 3600
    assert dec.DEFAULT_GROWTH_PER_H * 30 <= leak_per_h, "the bound must sit far below the leak"
    assert dec.DEFAULT_CEILING >= 50 * 5_790, "the ceiling must sit far above a healthy count"
    assert dec.DEFAULT_CEILING * 30 <= dec.WINDOWS_HANDLE_CAP, "and far below the 16.7M cap"
    assert dec.WINDOWS_HANDLE_CAP == 2 ** 24
    assert dec.DEFAULT_GROWTH_CONFIRM >= 3 and dec.DEFAULT_CEILING_CONFIRM >= 2


def test_unreachable_is_skip_and_keeps_the_reference():
    r = _run("", 1000, ref=("9224@1", 5790, 700), reachable=0)
    assert r["verdict"] == "SKIP"
    assert (r["next_ident"], r["next_handles"], r["next_epoch"]) == ("9224@1", 5790, 700)


@pytest.mark.parametrize("body", [
    json.dumps({"obs_version": "32.2.0"}),        # an older server / no readable OBS
    json.dumps({"obs_handles": "lots", "obs_handles_pid": "1"}),
    json.dumps({"obs_handles": "-4", "obs_handles_pid": "1"}),
    "not json", "[]", "",
])
def test_an_absent_or_garbled_facet_is_unknown_never_a_false_zero(body):
    r = _run(body, 1000, ref=("9224@1", 5790, 700))
    assert r["verdict"] == "UNKNOWN"
    assert (r["next_ident"], r["next_handles"], r["next_epoch"]) == ("9224@1", 5790, 700)


def test_the_first_sample_of_a_process_is_a_baseline():
    r = _run(_body(5790, pid="5748", start="1791197100"), 2000)
    assert r["verdict"] == "BASELINE"
    assert (r["next_ident"], r["next_handles"], r["next_epoch"]) == ("5748@1791197100", 5790, 2000)


def test_a_restart_resets_the_baseline_even_from_a_leaking_reference():
    ref = (f"9224@{STREAM_START}", 4_066_772, 1000)
    r = _run(_body(5806, pid="5748", start="1791197100"), 1300, ref=ref)
    assert r["verdict"] == "BASELINE"
    assert r["next_ident"] == "5748@1791197100" and r["next_handles"] == 5806


def test_a_reused_pid_with_a_new_start_is_a_restart_too():
    ref = ("5748@1791197100", 5790, 1000)
    r = _run(_body(5790, pid="5748", start="1791299999"), 1300, ref=ref)
    assert r["verdict"] == "BASELINE"


def test_flat_healthy_stream_reads_healthy():
    ref = ("5748@1791197100", 5806, 1000)
    for i, h in enumerate([5790, 5788, 5790, 5789, 5791], start=1):
        r = _run(_body(h, pid="5748", start="1791197100"), 1000 + 300 * i, ref=ref)
        assert r["verdict"] == "HEALTHY", (i, h, r)
        ref = (r["next_ident"], r["next_handles"], r["next_epoch"])


def test_the_5_10_leak_rate_reads_growing():
    ident = f"9224@{STREAM_START}"
    h0 = 5790
    r = _run(_body(int(h0 + LEAK_PER_S * 300)), 1300, ref=(ident, h0, 1000))
    assert r["verdict"] == "GROWING"
    assert r["rate_per_h"] == pytest.approx(LEAK_PER_S * 3600, rel=0.001)
    assert r["interval_s"] == 300
    # (cap - handles) / rate: ~99 h from a fresh OBS, the ticket's "~100 h after its start"
    assert 95 < r["hours_to_cap"] < 100


def test_a_one_off_step_reads_growing_once_then_healthy():
    ident = "5748@1791197100"
    r1 = _run(_body(7800, pid="5748", start="1791197100"), 1300, ref=(ident, 5790, 1000))
    assert r1["verdict"] == "GROWING"
    r2 = _run(_body(7801, pid="5748", start="1791197100"), 1600,
              ref=(r1["next_ident"], r1["next_handles"], r1["next_epoch"]))
    assert r2["verdict"] == "HEALTHY"


def test_growth_just_under_the_bound_is_healthy():
    ident = "5748@1791197100"
    per_pass = dec.DEFAULT_GROWTH_PER_H * 300 / 3600
    r = _run(_body(int(5790 + per_pass) - 1, pid="5748", start="1791197100"), 1300,
             ref=(ident, 5790, 1000))
    assert r["verdict"] == "HEALTHY"


def test_a_short_interval_holds_and_keeps_the_older_reference():
    ident = f"9224@{STREAM_START}"
    r = _run(_body(9000), 1100, ref=(ident, 5790, 1000))
    assert r["verdict"] == "HOLD"
    assert (r["next_ident"], r["next_handles"], r["next_epoch"]) == (ident, 5790, 1000)


def test_a_clock_step_back_rebases():
    ident = f"9224@{STREAM_START}"
    r = _run(_body(5790), 900, ref=(ident, 5790, 1000))
    assert r["verdict"] == "BASELINE"
    assert r["next_epoch"] == 900


def test_the_5_10_snapshot_is_over_the_ceiling():
    r = _run(_body(4_066_772), 1000)
    assert r["verdict"] == "CEILING"
    assert r["ceiling"] == dec.DEFAULT_CEILING
    assert r["next_handles"] == 4_066_772


def test_the_ceiling_wins_over_growth_and_still_tracks_the_rate():
    ident = f"9224@{STREAM_START}"
    r = _run(_body(4_080_834), 1300, ref=(ident, 4_066_772, 1000))
    assert r["verdict"] == "CEILING"
    assert r["rate_per_h"] == pytest.approx(168_744, rel=0.001)
    assert 75 < r["hours_to_cap"] < 76


def test_a_linux_box_pages_at_80_percent_of_its_soft_open_files_limit():
    r = _run(_body(819, pid="4242", start="1791190000", limit="1024"), 1000)
    assert r["verdict"] == "CEILING" and r["ceiling"] == 819 and r["cap"] == 1024
    r = _run(_body(818, pid="4242", start="1791190000", limit="1024"), 1000)
    assert r["verdict"] == "BASELINE"


def test_a_large_linux_limit_never_raises_the_ceiling_above_the_absolute_one():
    r = _run(_body(400_000, pid="4242", start="1", limit="524288"), 1000)
    assert r["ceiling"] == 419_430 and r["verdict"] == "BASELINE"
    r = _run(_body(400_000, pid="4242", start="1", limit="1048576"), 1000)
    assert r["ceiling"] == dec.DEFAULT_CEILING


def test_a_missing_start_time_keys_on_the_pid_alone():
    r = _run(_body(5790, pid="5748", start=""), 1000)
    assert r["next_ident"] == "5748"


def test_the_cli_prints_every_key_the_watchdog_reads():
    body = _body(int(5790 + LEAK_PER_S * 300))
    out = subprocess.run(
        [sys.executable, str(_MOD_PATH), "analyze", "--box-reachable", "1",
         "--now-epoch", "1300", "--ref-ident", f"9224@{STREAM_START}", "--ref-handles", "5790",
         "--ref-epoch", "1000"],
        input=body, capture_output=True, text=True, check=True).stdout
    kv = dict(line.split("=", 1) for line in out.splitlines())
    assert kv["verdict"] == "GROWING"
    assert kv["handles"] == str(int(5790 + LEAK_PER_S * 300))
    # int(5790 + 46.875 * 300) = 19852 -> +14062 handles in 300 s = 168,744/h
    assert kv["pid"] == "9224" and kv["rate_per_h"] == "168744"
    assert kv["next_ident"] == f"9224@{STREAM_START}" and kv["next_epoch"] == "1300"
    for key in ("start", "ident", "limit", "cap", "ceiling", "interval_s", "hours_to_cap",
                "next_handles"):
        assert key in kv


def test_the_cli_with_an_empty_reference_and_unreachable_box():
    out = subprocess.run(
        [sys.executable, str(_MOD_PATH), "analyze", "--box-reachable", "0", "--now-epoch", "5",
         "--ref-ident", "", "--ref-handles", "", "--ref-epoch", ""],
        input="", capture_output=True, text=True, check=True).stdout
    kv = dict(line.split("=", 1) for line in out.splitlines())
    assert kv["verdict"] == "SKIP" and kv["next_ident"] == "" and kv["next_epoch"] == ""
