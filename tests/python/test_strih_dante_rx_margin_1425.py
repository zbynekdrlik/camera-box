"""Issue 1425 (design 7107436350, obs#96): the Dante RX hand-off margin on strih-snv.

obs v0.4.61's dante-input takes `rx_stamp_margin_ms` (0..=50 ms, unset = 0): every RX chunk is stamped
that much after dlink's play time, so a chunk handed to OBS after its tick's end still makes OBS's FIXED
0 ms mix instead of being sat out (a ~21 ms gap in the program audio). obs's 2 h read of strih-snv on
v0.4.61 without it: late_to_mix +60, worst min_lead -7.0 ms. The box fact
`STRIH_DANTE_RX_STAMP_MARGIN_MS` = 10 on strih-snv, `none` everywhere else.

Covers the fact in the ONE loader, the TOML line, verify item 9's TOML row and the start-line row. Every
bash run is under the callers' `set -euo pipefail` (the issue-1513 harness).
"""
import sys
import tomllib
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_strih_dante_terms_1513 as t  # noqa: E402
import vban_leg_fakes_1439 as fk  # noqa: E402

KEY = "STRIH_DANTE_RX_STAMP_MARGIN_MS"


# --- the fact -----------------------------------------------------------------------------------------

def test_the_margin_follows_the_dante_terms_in_the_key_list():
    r = t._bash("strih_box_fact_keys")
    assert r.returncode == 0, r.stderr
    keys = r.stdout.split()
    assert keys[keys.index("STRIH_DANTE_TX_TRACK") + 1] == KEY


def test_only_strih_snv_declares_the_10_ms_margin():
    assert t._facts("strih-snv")[KEY] == "10"
    for box in ("stream-snv", "strih-pp", "stream-pp"):
        assert t._facts(box)[KEY] == "none", box


def test_every_box_loads_with_the_margin():
    r = t._bash('for b in strih-snv stream-snv strih-pp stream-pp; do (strih_box_load "$b" >/dev/null) || exit 1; done; '
                'strih_box_load strih-snv >/dev/null; strih_box_fact %s' % KEY)
    assert r.returncode == 0 and r.stdout.strip() == "10", r.stdout + r.stderr


@pytest.mark.parametrize("value", ["none", "0", "4.667", "10", "50", "50.0", "12.041666"])
def test_a_valid_margin_loads(tmp_path, value):
    r = t._load(tmp_path, "strih-snv", {KEY: value})
    assert r.returncode == 0, r.stderr


@pytest.mark.parametrize("value", ["51", "50.1", "-1", "x", "05", "00.5", ".5", "10.", "1e1", "10,5",
                                   "10.1234567", "١٠", "10 ms"])
def test_a_malformed_margin_is_refused(tmp_path, value):
    r = t._load(tmp_path, "strih-snv", {KEY: value})
    assert r.returncode != 0 and KEY in r.stderr, r.stderr


def test_a_box_without_a_program_device_sets_the_margin_none(tmp_path):
    r = t._load(tmp_path, "strih-pp", {KEY: "10"}, fk.pp_pre_1536_boxes_dir())
    assert r.returncode != 0 and KEY in r.stderr and "no " in r.stderr, r.stderr


# --- the toml -----------------------------------------------------------------------------------------------

def test_strih_snv_toml_states_the_margin_as_a_float_before_the_subscriptions(tmp_path):
    r = t._render(tmp_path, "strih-snv", "enp7s0.70")
    assert r.returncode == 0, r.stderr
    d = tomllib.loads(r.stdout)
    assert d["rx_stamp_margin_ms"] == 10.0 and isinstance(d["rx_stamp_margin_ms"], float)
    head = r.stdout.split("[[subscribe]]", 1)[0]
    assert "rx_stamp_margin_ms = 10.0\n" in head
    assert r.stdout.count("rx_stamp_margin_ms") == 1


def test_a_none_margin_writes_no_line(tmp_path):
    r = t._render(tmp_path, "stream-snv", "enp56s0.70")
    assert r.returncode == 0, r.stderr
    assert "rx_stamp_margin_ms" not in r.stdout


# --- verify item 9 -------------------------------------------------------------------------------------------

def test_item9_passes_strih_snv_with_its_margin_in_the_toml_and_the_log(tmp_path):
    r = t._strih(tmp_path)
    assert "BAD" not in r.stdout, r.stdout
    assert "rx_stamp_margin_ms = 10.0" in r.stdout, r.stdout   # the latency terms OK row names it
    assert "OK (program-dante) rx stamp margin 10.000 ms" in r.stdout, r.stdout


def test_item9_fails_a_toml_without_the_declared_margin(tmp_path):
    r = t._strih(tmp_path, toml=t.TOML_STRIH.replace("rx_stamp_margin_ms = 10.0\n", ""))
    assert "BAD (program-dante) latency terms" in r.stdout and "rx_stamp_margin_ms" in r.stdout, r.stdout


def test_item9_fails_a_toml_with_another_margin(tmp_path):
    r = t._strih(tmp_path, toml=t.TOML_STRIH.replace("rx_stamp_margin_ms = 10.0", "rx_stamp_margin_ms = 4.667"))
    assert "BAD (program-dante) latency terms" in r.stdout, r.stdout


def test_item9_fails_a_margin_the_facts_do_not_declare(tmp_path):
    toml = t.TOML_STREAM.replace("tx_track = 1\n", "tx_track = 1\nrx_stamp_margin_ms = 10.0\n")
    r = t._stream(tmp_path, toml=toml)
    assert "BAD (program-dante) latency terms" in r.stdout and "rx_stamp_margin_ms" in r.stdout, r.stdout


def _strih_log(tmp_path, margin_line):
    lines = [t._start("strih-obs", 0), t.SPLIT] + ([margin_line] if margin_line else []) + [
        t.NETLAT, t._status("12:00:10.000", 4800, tx_track="0", tx_played="0")]
    nxt = t._status("12:00:20.000", 484800, tx_track="0", tx_played="0")
    return t._grade(tmp_path, "strih-snv", lines, nxt, t.TOML_STRIH)


def test_item9_fails_an_obs_that_started_without_the_margin(tmp_path):
    r = _strih_log(tmp_path, None)
    assert "BAD (program-dante) no 'dante-input start: every RX chunk is stamped' line" in r.stdout, r.stdout
    assert "restart OBS" in r.stdout, r.stdout


def test_item9_fails_an_obs_that_runs_another_margin(tmp_path):
    r = _strih_log(tmp_path, t.MARGIN_LINE.replace("10.000 ms", "4.667 ms"))
    assert "BAD (program-dante) rx stamp margin 4.667 ms, the facts say 10.000 ms" in r.stdout, r.stdout


def test_item9_fails_a_margin_in_the_log_of_a_box_without_one(tmp_path):
    r = t._stream(tmp_path, extra=(t.SPLIT, t.TXLINE, t.MARGIN_LINE, t.NETLAT))
    assert "BAD (program-dante) rx stamp margin 10.000 ms on a device whose facts declare none" in r.stdout, r.stdout


def test_item9_names_no_margin_row_on_a_box_without_one(tmp_path):
    r = t._stream(tmp_path)
    assert "BAD" not in r.stdout, r.stdout
    assert "OK (program-dante) rx stamp margin: none (no start line, as the facts declare)" in r.stdout, r.stdout
