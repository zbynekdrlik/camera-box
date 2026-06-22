"""#149: unit tests for the obs_phase2 self-verify guard.

The harness (scripts/obs_phase2.py) MUST measure the EXACT certified production NDI
config — never a divergent one. These tests pin the pure comparison logic
(_diverging_locked_keys) and the fail-fast guard (_assert_probe_matches_prod) that
together make the MACHINE catch a config drift between the harness and prod, instead
of relying on a human to notice after a misconfigured (silently-invalid) run.

The bug #149 closes: the probe ingest ran ndi_sync=1 (network/receiver timing) while
the prod genlock cam inputs run ndi_sync=2 (source timing). With ndi_sync=1 the #136
timestamp-aligned release silently no-ops, so the harness "proved" a path it never
exercised. The guard asserts the probe's locked baseline equals prod's, ndi_sync
included, and FAILS FAST on any mismatch.
"""
import importlib.util
import pathlib
import sys

import pytest

# Import scripts/obs_phase2.py by path (it is a script, not an installed package).
_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


# The certified prod genlock cam input config, as read live from strih (2026-06-22):
# NDI cam1/3/5 = ndi_sync=2, genlock_fifo=True, ndi_bw_mode=0, latency=0, genlock_preload=1.
CERTIFIED_PROD = {
    "ndi_source_name": "CAM1 (usb)",
    "ndi_sync": 2,
    "genlock_fifo": True,
    "ndi_bw_mode": 0,
    "latency": 0,
    "genlock_preload": 1,
}


def _probe_from_baseline(**overrides):
    """A probe-effective settings dict built from the certified _PROBE_NDI_SETTINGS
    baseline (so the test tracks the real source of truth) with optional overrides."""
    s = dict(obs_phase2._PROBE_NDI_SETTINGS)
    s["ndi_source_name"] = "CAM1 (usb)"
    s.update(overrides)
    return s


# ---------------------------------------------------------------------------
# _diverging_locked_keys — the pure comparison core
# ---------------------------------------------------------------------------

def test_locked_baseline_keys_are_exactly_the_intended_set():
    # Lock the set so a future edit that drops ndi_sync (the #149 key) from the
    # guard fails this test loudly.
    assert obs_phase2._LOCKED_BASELINE_KEYS == (
        "ndi_sync", "genlock_fifo", "ndi_bw_mode", "latency",
    )


def test_probe_baseline_uses_source_timing_ndi_sync_2():
    # #149 GREEN behaviour: the shipped baseline must be ndi_sync=2 (source timing).
    # If someone reverts it to 1 (network timing), this fails.
    assert obs_phase2._PROBE_NDI_SETTINGS["ndi_sync"] == 2


def test_no_divergence_when_probe_mirrors_prod():
    probe = _probe_from_baseline()
    assert obs_phase2._diverging_locked_keys(CERTIFIED_PROD, probe) == []


def test_ndi_sync_mismatch_is_reported_with_expected_and_actual():
    # The exact #149 bug: probe on network timing (1) vs prod source timing (2).
    probe = _probe_from_baseline(ndi_sync=1)
    diverging = obs_phase2._diverging_locked_keys(CERTIFIED_PROD, probe)
    assert diverging == [{"key": "ndi_sync", "expected": 2, "actual": 1}]


def test_multiple_locked_keys_diverge_all_reported():
    probe = _probe_from_baseline(ndi_sync=1, genlock_fifo=False, latency=2)
    keys = {d["key"] for d in obs_phase2._diverging_locked_keys(CERTIFIED_PROD, probe)}
    assert keys == {"ndi_sync", "genlock_fifo", "latency"}


def test_missing_locked_key_in_probe_counts_as_divergence():
    probe = _probe_from_baseline()
    del probe["latency"]
    diverging = obs_phase2._diverging_locked_keys(CERTIFIED_PROD, probe)
    assert diverging == [{"key": "latency", "expected": 0, "actual": None}]


def test_preload_difference_is_NOT_a_divergence():
    # genlock_preload is per-source tuning (copied, _GENLOCK_COPY_KEYS) — allowed to
    # differ. It is NOT a locked baseline key, so a difference must be ignored.
    probe = _probe_from_baseline(genlock_preload=5)  # prod has 1
    assert obs_phase2._diverging_locked_keys(CERTIFIED_PROD, probe) == []


def test_extra_non_locked_probe_keys_are_ignored():
    probe = _probe_from_baseline(some_unrelated_distroav_key="x")
    assert obs_phase2._diverging_locked_keys(CERTIFIED_PROD, probe) == []


# ---------------------------------------------------------------------------
# _assert_probe_matches_prod — the fail-fast guard
# ---------------------------------------------------------------------------

def test_guard_passes_silently_when_probe_matches_prod():
    # No exception when the locked baseline matches.
    obs_phase2._assert_probe_matches_prod(
        "strih", "NDI cam5", CERTIFIED_PROD, _probe_from_baseline()
    )


def test_guard_aborts_on_ndi_sync_divergence_with_precise_diagnostic():
    probe = _probe_from_baseline(ndi_sync=1)
    with pytest.raises(SystemExit) as ei:
        obs_phase2._assert_probe_matches_prod("strih", "NDI cam5", CERTIFIED_PROD, probe)
    msg = str(ei.value)
    assert "#149 self-verify FAIL" in msg
    assert "ndi_sync" in msg
    # The diagnostic names both the certified-prod value and the probe value.
    assert "prod(certified)=2" in msg
    assert "probe=1" in msg


def test_guard_aborts_when_no_certified_prod_baseline_found():
    # No certified prod input → cannot verify → MUST abort (never measure an
    # unconfirmed config). This is the #149 core guarantee.
    with pytest.raises(SystemExit) as ei:
        obs_phase2._assert_probe_matches_prod("strih", None, {}, _probe_from_baseline())
    assert "#149 self-verify ABORT" in str(ei.value)


def test_guard_does_not_treat_preload_difference_as_failure():
    # The probe keeps prod's locked baseline but a different per-source preload.
    probe = _probe_from_baseline(genlock_preload=9)
    obs_phase2._assert_probe_matches_prod("strih", "NDI cam5", CERTIFIED_PROD, probe)


# ---------------------------------------------------------------------------
# _is_genlock_prod_input — only a genlocked prod input is a valid baseline
# ---------------------------------------------------------------------------

def test_genlock_input_is_recognised_as_certified_baseline():
    assert obs_phase2._is_genlock_prod_input(CERTIFIED_PROD) is True


def test_non_genlock_input_is_rejected_as_baseline():
    # A non-genlock NDI input (e.g. 'NDI 2ME PVW', 'NDI Bible') runs ndi_sync=1 and has
    # no genlock_fifo — matching it for the baseline would re-introduce the #149 bug.
    non_genlock = {"ndi_source_name": "STRIH-SNV (2ME PVW)", "ndi_sync": 1, "latency": 1}
    assert obs_phase2._is_genlock_prod_input(non_genlock) is False
