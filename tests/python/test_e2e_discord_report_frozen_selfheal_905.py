"""issue 905 item 2 — frozen_leg / self_heal_reset flip from REPORT-ONLY to BLOCKING
(`gates_overall_pass=true` on each node's JSON block). Mirrors the dup_cadence / own_burn_absent /
tear precedent (test_e2e_discord_report_dup_cadence_1166.py etc.): `_blocking_failures` gets a new
branch guarded by the node's own `gates_overall_pass` field, and `_report_only_tripped`'s existing
branch is guarded `gates_overall_pass is not True` so the classifier auto-follows the flip without
double-counting.

Two correctness details this test pins that the naive mirror gets wrong (recording-verdict.rs
`SelfHealAttributionReport`): `frozen` (hard-frozen windows) gates overall_pass but `stale_replay`
does NOT (`any_frozen()` reads only `frozen`), and self-heal gates on `attributed` OR
`unattributed_events` (`any_self_heal()` reads both), not `attributed` alone."""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402


def _base(overall_pass):
    return {
        "overall_pass": overall_pass,
        "full_chain": {"zero_loss": True},
        "all_cambox_continuity": {"overall_pass": True},
    }


def _frozen_block(gates_overall_pass, *, frozen=(), stale_replay=()):
    return {
        "frozen": list(frozen),
        "stale_replay": list(stale_replay),
        "gates_overall_pass": gates_overall_pass,
        "gate": (
            "blocking -- a genuinely frozen leg or a self-heal reset event fails overall_pass"
            if gates_overall_pass
            else "report-only -- does NOT gate overall_pass, pending cam1 hardware fix"
        ),
    }


def _self_heal_block(gates_overall_pass, *, attributed=(), unattributed=()):
    return {
        "attributed": list(attributed),
        "unattributed_events": list(unattributed),
        "gates_overall_pass": gates_overall_pass,
    }


def _frozen_leg_entry(cambox="cam2"):
    return {"cambox": cambox, "since_ns": 1, "copies": 149, "approx_stale_secs": 5.3,
            "density": 0.18, "message": f"{cambox} frozen"}


def _self_heal_entry(cambox="cam2"):
    return {"kind": "self_heal_reset", "cambox": cambox, "since_ns": 1, "reset_at_ns": 2,
            "copies": 149, "approx_stale_secs": 5.3, "density": 0.18, "message": f"{cambox} reset"}


def _unattributed_entry(cambox="cam2"):
    return {"kind": "self_heal_reset", "cambox": cambox, "at_ns": 3}


def _has(labels, sub):
    return any(sub in label.lower() for label, _ in labels)


# ---- LIVE (post-flip, gates_overall_pass=True) -> BLOCKING -------------------------------------

def test_frozen_live_is_blocking_and_not_report_only():
    v = _base(overall_pass=False)
    v["frozen_leg"] = _frozen_block(True, frozen=[_frozen_leg_entry()])
    v["self_heal_reset"] = _self_heal_block(True)
    assert _has(edr._blocking_failures(v), "zamrzn"), edr._blocking_failures(v)
    assert not any("zamrzn" in n.lower() or "stale" in n.lower()
                   for n in edr._report_only_tripped(v)), edr._report_only_tripped(v)
    # exercise routing through compose_summary, not just the overall_pass headline
    summary = edr.compose_summary(v, {"run_id": "905"})
    assert "❌" in summary
    assert "Zamrznutá kamera" in summary, summary


def test_self_heal_attributed_live_is_blocking_and_not_report_only():
    v = _base(overall_pass=False)
    v["frozen_leg"] = _frozen_block(True)
    v["self_heal_reset"] = _self_heal_block(True, attributed=[_self_heal_entry()])
    assert _has(edr._blocking_failures(v), "self-heal"), edr._blocking_failures(v)
    assert not any("self-heal" in n.lower() for n in edr._report_only_tripped(v)), \
        edr._report_only_tripped(v)
    summary = edr.compose_summary(v, {"run_id": "905"})
    assert "❌" in summary
    assert "Self-heal reset" in summary, summary


def test_self_heal_UNattributed_only_live_is_blocking():
    # any_self_heal() gates on attributed OR unattributed_events -- the naive mirror (attributed
    # only) would silently miss a run whose ONLY self-heal signal is an unattributed reset event.
    v = _base(overall_pass=False)
    v["frozen_leg"] = _frozen_block(True)
    v["self_heal_reset"] = _self_heal_block(True, unattributed=[_unattributed_entry()])
    assert _has(edr._blocking_failures(v), "self-heal"), edr._blocking_failures(v)


# ---- pre-flip (gates_overall_pass=False) -> stays REPORT-ONLY ----------------------------------

def test_frozen_pre_flip_stays_report_only():
    v = _base(overall_pass=True)
    v["frozen_leg"] = _frozen_block(False, frozen=[_frozen_leg_entry()])
    v["self_heal_reset"] = _self_heal_block(False)
    assert any("zamrzn" in n.lower() for n in edr._report_only_tripped(v)), \
        edr._report_only_tripped(v)
    assert not _has(edr._blocking_failures(v), "zamrzn"), edr._blocking_failures(v)
    assert "❌" not in edr.compose_summary(v, {"run_id": "905"})


def test_self_heal_pre_flip_stays_report_only():
    v = _base(overall_pass=True)
    v["frozen_leg"] = _frozen_block(False)
    v["self_heal_reset"] = _self_heal_block(False, attributed=[_self_heal_entry()])
    assert any("self-heal" in n.lower() for n in edr._report_only_tripped(v)), \
        edr._report_only_tripped(v)
    assert not _has(edr._blocking_failures(v), "self-heal"), edr._blocking_failures(v)


# ---- stale_replay NEVER gates overall_pass, even post-flip -> always report-only ---------------

def test_stale_replay_only_stays_report_only_even_when_gates_true():
    # stale_replay is NOT part of any_frozen(), so it must NEVER be a blocking failure -- it stays
    # report-only regardless of the frozen_leg node's gates_overall_pass flag.
    v = _base(overall_pass=True)
    v["frozen_leg"] = _frozen_block(True, stale_replay=[{"cambox": "cam2", "copies": 3,
                                                         "message": "cam2 stale"}])
    v["self_heal_reset"] = _self_heal_block(True)
    assert any("stale" in n.lower() for n in edr._report_only_tripped(v)), \
        edr._report_only_tripped(v)
    assert not _has(edr._blocking_failures(v), "zamrzn"), edr._blocking_failures(v)


# ---- clean -> never listed either way ----------------------------------------------------------

def test_clean_is_never_listed_either_way():
    for gop in (True, False):
        v = _base(overall_pass=True)
        v["frozen_leg"] = _frozen_block(gop)
        v["self_heal_reset"] = _self_heal_block(gop)
        assert not any("zamrzn" in n.lower() or "stale" in n.lower() or "self-heal" in n.lower()
                       for n in edr._report_only_tripped(v)), (gop, edr._report_only_tripped(v))
        assert not (_has(edr._blocking_failures(v), "zamrzn")
                    or _has(edr._blocking_failures(v), "self-heal")), (gop, edr._blocking_failures(v))


if __name__ == "__main__":
    import traceback
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    failed = 0
    for fn in fns:
        try:
            fn()
            print("PASS", fn.__name__)
        except Exception:
            failed += 1
            print("FAIL", fn.__name__)
            traceback.print_exc()
    sys.exit(1 if failed else 0)
