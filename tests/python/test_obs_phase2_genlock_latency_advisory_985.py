"""#985: unit tests for the obs_phase2 genlock_latency_ms_src ADVISORY.

`genlock_latency_ms_src` is DELIBERATELY excluded from `_LOCKED_BASELINE_KEYS` (issue 149's
certified-baseline self-verify) -- a probe measurement must stay usable even when the
`phase2-probe-src` input sits at OBS's build default (3ms) while the certified prod input runs
its calibrated A/V-align hold (948ms, live-read on stream's 'NDI 2ME PGM' 2026-08-05). But that
same exclusion means a probe run can SILENTLY diverge from prod's A/V timing by nearly a second
with nobody told. `_genlock_latency_advisory` makes that divergence LOUD (a non-fatal log line)
instead of silent -- unlike `_assert_probe_matches_prod`, it never aborts the run.
"""
import importlib.util
import pathlib
import sys

# Import scripts/obs_phase2.py by path (it is a script, not an installed package).
_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


def test_genlock_latency_advisory_key_matches_the_real_obs_property_name():
    # Lock the key name so a future rename of the OBS property (_GENLOCK_SRC_LATENCY_KEY, the
    # #358 constant) can't silently desync the advisory from what it claims to compare.
    assert (
        obs_phase2._GENLOCK_LATENCY_ADVISORY_KEY
        == obs_phase2._GENLOCK_SRC_LATENCY_KEY
        == "genlock_latency_ms_src"
    )


def test_no_advisory_when_probe_matches_prod():
    prod = {"genlock_latency_ms_src": 948}
    probe = {"genlock_latency_ms_src": 948}
    assert obs_phase2._genlock_latency_advisory("stream", "NDI 2ME PGM", prod, probe) is None


def test_advisory_fires_on_the_live_985_incident_values():
    # The exact live divergence from issue 985: prod calibrated to 948ms, the probe input still
    # at OBS's 3ms build default.
    prod = {"genlock_latency_ms_src": 948}
    probe = {"genlock_latency_ms_src": 3}
    advisory = obs_phase2._genlock_latency_advisory("stream", "NDI 2ME PGM", prod, probe)
    assert advisory is not None
    assert "948" in advisory
    assert "3" in advisory
    assert "NDI 2ME PGM" in advisory
    assert "stream" in advisory
    # Must explicitly warn against taking an A/V reading from the probe path -- the whole point
    # (issue 985's dispatch framing: "so nobody takes an A/V reading from the probe path by
    # mistake").
    assert "A/V" in advisory


def test_no_advisory_when_prod_value_unknown():
    # A prod read failure (empty certified dict, or the key genuinely absent) must never fire a
    # false advisory -- _assert_probe_matches_prod already hard-aborts the run in that case, so
    # this path is unreachable in practice, but the pure function itself must stay defensive.
    prod = {}
    probe = {"genlock_latency_ms_src": 3}
    assert obs_phase2._genlock_latency_advisory("stream", "NDI 2ME PGM", prod, probe) is None


def test_no_advisory_when_probe_value_unknown():
    prod = {"genlock_latency_ms_src": 948}
    probe = {}
    assert obs_phase2._genlock_latency_advisory("stream", "NDI 2ME PGM", prod, probe) is None


def test_advisory_is_non_fatal_never_raises():
    # Unlike _assert_probe_matches_prod (SystemExit on divergence), this is a pure function that
    # only ever returns a string or None -- never raises, by design (issue 985: the divergence is
    # INTENTIONAL/allowed, only the silence around it is the bug).
    prod = {"genlock_latency_ms_src": 948}
    probe = {"genlock_latency_ms_src": 3}
    # Would raise here if this were _assert_probe_matches_prod-shaped.
    result = obs_phase2._genlock_latency_advisory("stream", "NDI 2ME PGM", prod, probe)
    assert isinstance(result, str)
