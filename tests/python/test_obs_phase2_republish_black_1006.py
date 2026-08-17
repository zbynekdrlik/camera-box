"""#1006 — the DIFFERENTIAL republish-black detector: page ONLY when a live upstream NDI feed is
being republished as BLACK Spout, never when the upstream is itself idle.

## The bug this observes (#1006, measured live 2026-08-06 + re-confirmed 2026-08-17)

strih's `CG bridge` scene (one item, `spout CG`, fed by Resolume Arena's "CG_Bridge light"
composition over Spout) rendered fully BLACK on air with no alarm — every process/liveness check
said "clean" (Arena up, plugin up, sender registered). The receiver side was healthy: the sibling
`spout moderatori` on the SAME plugin rendered fine. The fault was INTERNAL to Arena — its CG-bridge
composition output produced no pixels while the SAME content it ingests (the direct NDI input `cg`,
`RESOLUME-SNV (cg-obs)`) rendered LIVE at peak 180.

## Why "every production scene must be non-black" (the ticket's literal scope) is wrong

Measured live on a HEALTHY rig (2026-08-17): `CG bridge` AND `Ableset` (lyrics) are BOTH legitimately
black at idle (no lower-third / no lyric triggered), while program (`Cam 1`) is live. So a blanket
"every scene non-black" check false-fails constantly on idle overlay scenes.

## The differential contract this locks

A republish is only a FAULT when its live upstream reference is delivering content but the republish
shows black — the exact 2026-08-06 signature. Both-black = legitimately idle = OK; both-live = OK;
reference-black = nothing to republish = IDLE (never an alarm). An unreadable screenshot = UNKNOWN
(never a silent OK).
"""
import argparse
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_rb", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_rb"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


# ---- the pure decision -------------------------------------------------------------------------

def test_fault_when_reference_live_but_subject_black():
    # The exact #1006 signature: upstream `cg` peak=180 live, `spout CG` peak=0 black.
    assert obs_phase2._republish_black_verdict(
        ref_max=180, ref_mean=38.4, subj_max=0, subj_mean=0.0) == "FAULT"


def test_ok_when_both_live():
    assert obs_phase2._republish_black_verdict(
        ref_max=200, ref_mean=90.0, subj_max=247, subj_mean=24.0) == "OK"


def test_idle_when_reference_black_regardless_of_subject():
    # Upstream not feeding -> a black republish is EXPECTED, never an alarm (the 2026-08-17 state).
    assert obs_phase2._republish_black_verdict(
        ref_max=0, ref_mean=0.0, subj_max=0, subj_mean=0.0) == "IDLE"
    # Even a (nonsensical) live subject over a black reference is not a republish-black fault.
    assert obs_phase2._republish_black_verdict(
        ref_max=0, ref_mean=0.0, subj_max=200, subj_mean=50.0) == "IDLE"


def test_unknown_when_either_screenshot_unreadable():
    assert obs_phase2._republish_black_verdict(
        ref_max=None, ref_mean=None, subj_max=0, subj_mean=0.0) == "UNKNOWN"
    assert obs_phase2._republish_black_verdict(
        ref_max=180, ref_mean=38.4, subj_max=None, subj_mean=None) == "UNKNOWN"


def test_min_mean_floor_treats_a_dim_only_reference_as_idle_not_live():
    # With a mean floor, a high-peak/near-black-mean reference (a mid-renegotiation garbage frame)
    # is NOT counted as a live upstream, so a black subject is IDLE, not a false FAULT.
    assert obs_phase2._republish_black_verdict(
        ref_max=117, ref_mean=2.7, subj_max=0, subj_mean=0.0, min_mean=20) == "IDLE"
    # Default (peak-only) keeps the ticket's own peak-based semantics: peak 180 = live -> FAULT.
    assert obs_phase2._republish_black_verdict(
        ref_max=180, ref_mean=2.7, subj_max=0, subj_mean=0.0) == "FAULT"


# ---- the subcommand handler --------------------------------------------------------------------

class _FakeWS:
    def close(self):
        pass


def _patch_ws(monkeypatch, luma_by_name):
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(
        obs_phase2, "_program_luma",
        lambda ws, name: luma_by_name.get(name, (None, None)),
    )


def _ns(**kw):
    base = dict(host="10.77.9.202", password="", reference="cg", subject="spout CG",
               min_mean=None, label="")
    base.update(kw)
    return argparse.Namespace(**base)


def test_handler_fault_exits_3(monkeypatch, capsys):
    _patch_ws(monkeypatch, {"cg": (180, 38.4), "spout CG": (0, 0.0)})
    with pytest.raises(SystemExit) as e:
        obs_phase2.republish_black_check(_ns())
    assert e.value.code == 3
    err = capsys.readouterr().err
    assert "FAULT" in err and "spout CG" in err and "cg" in err


def test_handler_idle_exits_0(monkeypatch, capsys):
    _patch_ws(monkeypatch, {"cg": (0, 0.0), "spout CG": (0, 0.0)})
    obs_phase2.republish_black_check(_ns())  # no SystemExit -> exit 0
    assert "IDLE" in capsys.readouterr().out


def test_handler_ok_exits_0(monkeypatch, capsys):
    _patch_ws(monkeypatch, {"cg": (200, 90.0), "spout CG": (247, 24.0)})
    obs_phase2.republish_black_check(_ns())
    assert "OK" in capsys.readouterr().out


def test_handler_unknown_exits_4(monkeypatch, capsys):
    _patch_ws(monkeypatch, {"cg": (None, None), "spout CG": (0, 0.0)})
    with pytest.raises(SystemExit) as e:
        obs_phase2.republish_black_check(_ns())
    assert e.value.code == 4
    assert "UNKNOWN" in capsys.readouterr().err


def test_subcommand_parses_and_dispatches(monkeypatch):
    captured = {}
    monkeypatch.setattr(obs_phase2, "republish_black_check",
                        lambda a: captured.update(reference=a.reference, subject=a.subject,
                                                  host=a.host, min_mean=a.min_mean))
    monkeypatch.setattr(sys, "argv", [
        "obs_phase2.py", "republish-black-check", "--host", "10.77.9.202",
        "--reference", "cg", "--subject", "spout CG", "--min-mean", "20",
    ])
    obs_phase2.main()
    assert captured == {"reference": "cg", "subject": "spout CG",
                        "host": "10.77.9.202", "min_mean": 20.0}
