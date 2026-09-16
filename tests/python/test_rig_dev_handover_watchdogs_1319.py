"""issue 1319 -- item 16 `watchdogs`: the development-handover check must verify the dev1
`--user` production-critical alert-watchdog timers are enabled + active + fired recently.

Root cause: `av-step-alert-watchdog.timer` + `avsync-lineup-alert-watchdog.timer` were NEVER
installed on dev1 (A/V-sync notifications silently never fired) and NOTHING reported it. This item
closes that gap. A disabled/inactive/never-run timer is the SUPERVISOR's to fix (attribution
`SUPERVISOR`, Slovak `nezapnutý watchdog: <names>`, exit 1), NEVER blamed on the owner; a timer with
no unit file reads UNKNOWN. All logic lives in the pure engine (Tier-0 #557: pytest, no cargo).
"""
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
sys.path.insert(0, str(_SCRIPTS))
import rig_dev_handover_decision as d  # noqa: E402


def _wd_line(name, scope="core", unit="present", enabled="yes", active="yes", age_s="30"):
    return "watchdog %s scope=%s unit=%s enabled=%s active=%s age_s=%s" % (
        name, scope, unit, enabled, active, age_s)


# --- classify_watchdogs ------------------------------------------------------------------------
def test_all_enabled_active_recent_is_ok():
    text = "\n".join(_wd_line(n) for n in ("av-step", "avsync-lineup", "network-reach"))
    r = d.classify_watchdogs(text)
    assert r["status"] == d.OK
    assert r["off"] == [] and r["missing"] == []
    assert "3 watchdog" in r["message"] or "3" in r["message"]


def test_disabled_or_inactive_or_neverrun_are_supervisor_off():
    text = "\n".join([
        _wd_line("av-step", enabled="yes", active="yes", age_s="30"),        # ok
        _wd_line("avsync-lineup", enabled="no", active="no", age_s="na"),      # disabled -> off
        _wd_line("audio-lag", enabled="yes", active="no", age_s="na"),         # inactive -> off
        _wd_line("genlock-lock", enabled="yes", active="yes", age_s="na"),     # never-run -> off
    ])
    r = d.classify_watchdogs(text)
    assert r["status"] == d.SUPERVISOR
    assert r["off"] == ["avsync-lineup", "audio-lag", "genlock-lock"]
    assert "nezapnutý watchdog:" in r["message"]
    for n in ("avsync-lineup", "audio-lag", "genlock-lock"):
        assert n in r["message"]
    assert "av-step" not in r["message"]  # the healthy one is not named


def test_stale_beyond_threshold_is_off():
    text = _wd_line("av-step", enabled="yes", active="yes", age_s="1200")  # >900
    r = d.classify_watchdogs(text)
    assert r["status"] == d.SUPERVISOR
    assert r["off"] == ["av-step"]


def test_missing_unit_file_is_unknown_with_names():
    text = "\n".join([
        _wd_line("av-step"),                                     # ok
        _wd_line("avsync-lineup", unit="absent", enabled="no"),  # no unit file
    ])
    r = d.classify_watchdogs(text)
    assert r["status"] == d.UNKNOWN
    assert r["missing"] == ["avsync-lineup"]
    assert "avsync-lineup" in r["message"]


def test_off_dominates_missing():
    text = "\n".join([
        _wd_line("a", enabled="no"),               # off
        _wd_line("b", unit="absent"),              # missing
    ])
    r = d.classify_watchdogs(text)
    assert r["status"] == d.SUPERVISOR
    assert r["off"] == ["a"] and r["missing"] == ["b"]
    assert "a" in r["message"] and "b" in r["message"]  # both surfaced to the owner


def test_no_lines_is_unknown():
    r = d.classify_watchdogs("systemctl missing -- cannot probe watchdogs")
    assert r["status"] == d.UNKNOWN
    assert r["off"] == [] and r["missing"] == []


def test_imag_retired_skips_imag_scoped_timer():
    text = "\n".join([
        _wd_line("core-wd", scope="core", enabled="yes", active="yes", age_s="30"),
        _wd_line("imag-obs", scope="imag", enabled="no", active="no", age_s="na"),  # would be off
    ])
    # imag live: the imag timer's disabled state IS a supervisor problem
    assert d.classify_watchdogs(text, imag_retired=False)["status"] == d.SUPERVISOR
    # imag retired (issue 1316): the imag timer is ignored -> only the healthy core timer remains
    r = d.classify_watchdogs(text, imag_retired=True)
    assert r["status"] == d.OK
    assert "imag-obs" not in r["message"]


# --- Item + build_checklist + evaluate integration ---------------------------------------------
def test_watchdogs_item_decode_supervisor():
    item = next(i for i in d.ITEMS if i.key == "watchdogs")
    text = "\n".join([
        _wd_line("av-step"),
        _wd_line("avsync-lineup", enabled="no", active="no", age_s="na"),
    ])
    entry = item.decide({"watchdogs": (text, 0)})
    assert entry["status"] == d.SUPERVISOR
    assert entry["names"] == ["avsync-lineup"]
    assert "nezapnutý watchdog: avsync-lineup" in entry["message"]


def test_build_checklist_supervisor_is_exit_1_and_names_timers():
    entries = [
        {"key": "mic", "label": "merací mikrofón (mbc)", "status": d.OK, "message": "ok"},
        {"key": "watchdogs", "label": "dev1 watchdog timery", "status": d.SUPERVISOR,
         "message": "nezapnutý watchdog: av-step, avsync-lineup",
         "names": ["av-step", "avsync-lineup"]},
    ]
    lines, summary, code = d.build_checklist(entries)
    assert code == 1
    assert "supervisor musí zapnúť" in summary
    assert "av-step" in summary and "avsync-lineup" in summary
    assert "zabudol si:" not in summary  # NOT blamed on the owner


def test_build_checklist_forgot_and_supervisor_both_present():
    entries = [
        {"key": "mode", "label": "rig režim (TEST/EVENT)", "status": d.FORGOT, "message": "x"},
        {"key": "watchdogs", "label": "dev1 watchdog timery", "status": d.SUPERVISOR,
         "message": "y", "names": ["av-step"]},
    ]
    _, summary, code = d.build_checklist(entries)
    assert code == 1
    assert summary.startswith("zabudol si: rig režim (TEST/EVENT)")
    assert "supervisor musí zapnúť: av-step" in summary


def test_evaluate_reads_watchdogs_capture(tmp_path):
    (tmp_path / "watchdogs.out").write_text(
        "\n".join([_wd_line("av-step"), _wd_line("avsync-lineup", enabled="no")]),
        encoding="utf-8")
    (tmp_path / "watchdogs.rc").write_text("0", encoding="utf-8")
    only = [i for i in d.ITEMS if i.key == "watchdogs"]
    entries, lines, summary, code = d.evaluate(str(tmp_path), items=only)
    assert code == 1
    assert entries[0]["status"] == d.SUPERVISOR
    assert "supervisor musí zapnúť" in summary


def test_evaluate_imag_retired_flag_threads_through(tmp_path):
    (tmp_path / "watchdogs.out").write_text(
        "\n".join([
            _wd_line("core-wd", scope="core"),
            _wd_line("imag-obs", scope="imag", enabled="no"),
        ]), encoding="utf-8")
    (tmp_path / "watchdogs.rc").write_text("0", encoding="utf-8")
    only = [i for i in d.ITEMS if i.key == "watchdogs"]
    _, _, _, code_live = d.evaluate(str(tmp_path), items=only, imag_retired=False)
    _, _, _, code_retired = d.evaluate(str(tmp_path), items=only, imag_retired=True)
    assert code_live == 1        # imag timer off = supervisor problem while imag lives
    assert code_retired == 0     # imag retired -> ignored, only healthy core timer remains


# --- issue 1316: a retired imag-nb drops its imag-scoped captures (pins_imag) ------------------
def _pins_item():
    return [i for i in d.ITEMS if i.key == "pins"][0]


def test_pins_imag_retired_drops_the_imag_capture_1316():
    pins = _pins_item()
    # imag pins DRIFT (rc 1) but strih/stream clean: live -> FORGOT (imag counts); retired -> OK.
    caps = {"pins_strih": ("", 0), "pins_stream": ("", 0), "pins_imag": ("", 1)}
    assert pins.decide(caps, imag_retired=False)["status"] == d.FORGOT
    assert pins.decide(caps, imag_retired=True)["status"] == d.OK


def test_pins_imag_retired_missing_capture_is_not_unknown_1316():
    pins = _pins_item()
    # When imag is retired the orchestrator SKIPS the pins_imag probe, so its capture is absent.
    # Live that absence -> UNKNOWN (a box we could not read); retired -> dropped -> OK on strih/stream.
    caps = {"pins_strih": ("", 0), "pins_stream": ("", 0)}
    assert pins.decide(caps, imag_retired=False)["status"] == d.UNKNOWN
    assert pins.decide(caps, imag_retired=True)["status"] == d.OK


def test_evaluate_reads_rdh_imag_retired_env_1316(monkeypatch, tmp_path):
    # main() reads RDH_IMAG_RETIRED from the environment; prove a truthy value threads to the pins item.
    (tmp_path / "pins_strih.out").write_text("", encoding="utf-8")
    (tmp_path / "pins_strih.rc").write_text("0", encoding="utf-8")
    (tmp_path / "pins_stream.out").write_text("", encoding="utf-8")
    (tmp_path / "pins_stream.rc").write_text("0", encoding="utf-8")
    (tmp_path / "pins_imag.out").write_text("", encoding="utf-8")
    (tmp_path / "pins_imag.rc").write_text("1", encoding="utf-8")  # imag drift
    only = _pins_item()
    entries_live, _, _, _ = d.evaluate(str(tmp_path), items=[only], imag_retired=False)
    entries_ret, _, _, _ = d.evaluate(str(tmp_path), items=[only], imag_retired=True)
    assert entries_live[0]["status"] == d.FORGOT
    assert entries_ret[0]["status"] == d.OK
