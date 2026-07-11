"""#358: unit tests for the genlock-latency-delivery hard gate.

The rescoped #358 requirement (2026-06-30 comment):
  1. SNAPSHOT the per-source genlock latency (genlock_latency_ms_src) + Render-Delay
     (gpu_delay) filters' enabled-state on the prod stream input ('NDI 2ME PGM').
  2. SET the configured test latency (default 1000ms) on that source and DISABLE its
     gpu_delay filters (so they don't mask the test value in the genlock-fifo audit).
  3. DELIVER-VERIFY (the #292 gate): the effective held latency reported by the
     genlock-fifo audit must be >= set_ms - tolerance (default 100ms). This catches the
     #292 silent-non-apply bug (force-drain held the FIFO at ~3-50ms even when 1000ms was
     set). The PASS/FAIL decision lives in a pure function so it is Tier-0 testable.
  4. RESTORE in the cleanup EXIT/HUP/INT/TERM trap with verified read-back + LOUD WARN if
     read-back differs from snapshot (mirrors the #246 burn-verify pattern).

These tests cover:
  a. _latency_delivery_ok() pure function — the #292 PASS/FAIL decision.
  b. _parse_latency_ms_from_audit_line() pure function — parses effective latency.
  c. Round-trip: snapshot→set→restore == snapshot (exact values, exact input).
  d. Restore is a no-op when nothing was snapshotted.
  e. Restore issues LOUD warn when GetInputSettings read-back ≠ snapshot.
  f. CLI: --test-latency-ms arg parsed and forwarded to prod_scene.
"""
import importlib.util
import pathlib
import sys

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_latency", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_latency"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

def _patch_capture(monkeypatch, rpc_response=None):
    """Patch _rpc to record calls; _save_state to no-op. Return the calls list."""
    calls = []

    def fake_rpc(ws, op, payload=None, ignore_err=False, timeout_s=None):
        calls.append((op, payload or {}))
        if rpc_response is not None:
            return rpc_response.get(op, {})
        return {}

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_save_state", lambda state: None)
    return calls


# ---------------------------------------------------------------------------
# (a) _latency_delivery_ok() pure function — the #292 PASS/FAIL decision
# ---------------------------------------------------------------------------

class TestLatencyDeliveryOk:
    def test_functions_exist(self):
        assert callable(obs_phase2._latency_delivery_ok)

    def test_pass_when_delivered_equals_set(self):
        assert obs_phase2._latency_delivery_ok(1000, 1000) is True

    def test_pass_when_delivered_within_tolerance(self):
        # 1000 set, 950 delivered — within 100ms tolerance
        assert obs_phase2._latency_delivery_ok(1000, 950) is True

    def test_pass_exactly_at_tolerance_boundary(self):
        # 1000 set, 900 delivered — exactly at 100ms tolerance
        assert obs_phase2._latency_delivery_ok(1000, 900) is True

    def test_fail_when_force_drained_to_near_zero(self):
        # #292 bug: FIFO force-drained to ~3ms even though 1000ms was set
        assert obs_phase2._latency_delivery_ok(1000, 3) is False

    def test_fail_when_force_drained_to_50ms(self):
        # #292 bug: FIFO held at ~50ms with 1000ms set (30fps drop-cap miscalc)
        assert obs_phase2._latency_delivery_ok(1000, 50) is False

    def test_fail_when_delivered_just_below_tolerance(self):
        # 1000 set, 899 delivered — 101ms short, just beyond tolerance
        assert obs_phase2._latency_delivery_ok(1000, 899) is False

    def test_pass_at_450ms_baseline_within_tolerance(self):
        # 450 set, 450 delivered — the prod A/V-align baseline passes its own gate
        assert obs_phase2._latency_delivery_ok(450, 450) is True

    def test_custom_tolerance(self):
        assert obs_phase2._latency_delivery_ok(1000, 850, tolerance_ms=200) is True
        assert obs_phase2._latency_delivery_ok(1000, 799, tolerance_ms=200) is False


# ---------------------------------------------------------------------------
# (b) _parse_latency_ms_from_audit_line() pure function
# ---------------------------------------------------------------------------

_AUDIT_LINE_1000 = (
    "10:00:00.000 INFO: genlock-fifo audit 'NDI 2ME PGM': received=100 consumed=99 "
    "underruns=0 holds=99 overruns=0 backward_steps=0 depth=32 peak=34 latency_ms=1000 "
    "(≈60 frames @ 60.000fps) src_latency_ms=1000 global_latency_ms=3 "
    "preload=1 (=17 ms) reserve_ms=0 cap=64"
)
_AUDIT_LINE_50 = (
    "10:00:05.000 INFO: genlock-fifo audit 'NDI 2ME PGM': received=100 consumed=99 "
    "underruns=2 holds=3 overruns=0 backward_steps=0 depth=2 peak=4 latency_ms=50 "
    "(≈3 frames @ 60.000fps) src_latency_ms=1000 global_latency_ms=3 "
    "preload=1 (=17 ms) reserve_ms=0 cap=64"
)
_AUDIT_LINE_DIFFERENT_SOURCE = (
    "10:00:05.000 INFO: genlock-fifo audit 'NDI cam5': received=100 consumed=99 "
    "underruns=0 holds=99 overruns=0 backward_steps=0 depth=32 peak=34 latency_ms=450 "
    "(≈27 frames @ 60.000fps) src_latency_ms=450 global_latency_ms=3 "
    "preload=31 (=517 ms) reserve_ms=0 cap=64"
)
_NON_AUDIT_LINE = "10:00:00.000 INFO: some other log line without the marker"


class TestParseLatencyMsFromAuditLine:
    def test_function_exists(self):
        assert callable(obs_phase2._parse_latency_ms_from_audit_line)

    def test_parses_1000ms(self):
        result = obs_phase2._parse_latency_ms_from_audit_line(_AUDIT_LINE_1000)
        assert result == 1000

    def test_parses_50ms_force_drained(self):
        result = obs_phase2._parse_latency_ms_from_audit_line(_AUDIT_LINE_50)
        assert result == 50

    def test_returns_none_for_non_audit_line(self):
        result = obs_phase2._parse_latency_ms_from_audit_line(_NON_AUDIT_LINE)
        assert result is None

    def test_picks_effective_latency_ms_not_src_latency_ms(self):
        # _AUDIT_LINE_50: latency_ms=50 but src_latency_ms=1000
        # Must return 50 (the EFFECTIVE held value), not 1000 (the setting).
        result = obs_phase2._parse_latency_ms_from_audit_line(_AUDIT_LINE_50)
        assert result == 50, (
            "must parse latency_ms= (effective held), NOT src_latency_ms= (the setting)"
        )


# ---------------------------------------------------------------------------
# (c) snapshot→set→restore round-trip
# ---------------------------------------------------------------------------

class TestLatencySnapshotSetRestore:
    def test_snapshot_and_set_functions_exist(self):
        assert callable(obs_phase2._snapshot_and_set_test_latency)
        assert callable(obs_phase2._restore_test_latency)
        assert hasattr(obs_phase2, "_TEST_LATENCY_STATE_KEY")

    def test_snapshot_saves_current_latency_and_sets_test_value(self, monkeypatch):
        rpc_response = {
            "GetInputSettings": {
                "inputSettings": {"genlock_latency_ms_src": 450},
            },
            "GetSourceFilterList": {
                "filters": [
                    {"filterName": "Render Delay", "filterKind": "gpu_delay",
                     "filterEnabled": True, "filterSettings": {"delay_ms": 500}},
                ],
            },
        }
        calls = _patch_capture(monkeypatch, rpc_response=rpc_response)
        state = {}
        obs_phase2._snapshot_and_set_test_latency(
            None, "10.0.0.1", "NDI 2ME PGM", 1000, state
        )

        # genlock_latency_ms_src was read (GetInputSettings)
        gets = [c for c in calls if c[0] == "GetInputSettings"]
        assert any(p.get("inputName") == "NDI 2ME PGM" for _, p in gets), \
            "must read current latency from 'NDI 2ME PGM'"

        # genlock_latency_ms_src was set to 1000
        sets = [c for c in calls if c[0] == "SetInputSettings"]
        latency_sets = [p for _, p in sets
                        if p.get("inputName") == "NDI 2ME PGM"
                        and "genlock_latency_ms_src" in p.get("inputSettings", {})]
        assert len(latency_sets) == 1, f"expected one latency SetInputSettings, got {sets}"
        assert latency_sets[0]["inputSettings"]["genlock_latency_ms_src"] == 1000

        # state persisted snapshot
        saved = state["10.0.0.1"][obs_phase2._TEST_LATENCY_STATE_KEY]
        assert saved["input"] == "NDI 2ME PGM"
        assert saved["latency_ms"] == 450, "snapshot must save the ORIGINAL prod value (450)"

    def test_691_snapshot_is_saved_even_when_already_at_the_target_value(self, monkeypatch):
        # #691 REGRESSION GUARD for the live incident: the box's current value ALREADY
        # equals the (auto-derived) test value -- nothing needs to be forced. The OLD
        # code took this to mean "nothing to restore" and DISCARDED any existing saved
        # snapshot; that is exactly how the stream box got stuck at 1000ms forever (a
        # PRIOR run's own restore never completed, so its leftover snapshot was the only
        # way back to 925 -- and the old code threw it away instead of using it).
        rpc_response = {
            "GetInputSettings": {
                "inputSettings": {"genlock_latency_ms_src": 925},
            },
            "GetSourceFilterList": {"filters": []},
        }
        calls = _patch_capture(monkeypatch, rpc_response=rpc_response)
        # A PRE-EXISTING (stale, from an earlier incomplete run) snapshot for this host.
        state = {
            "10.0.0.1": {
                obs_phase2._TEST_LATENCY_STATE_KEY: {
                    "input": "NDI 2ME PGM", "latency_ms": 3, "render_delays": [],
                }
            }
        }
        # requested_test_latency_ms=925 == current value -> the "nothing to force" branch.
        changed = obs_phase2._snapshot_and_set_test_latency(
            None, "10.0.0.1", "NDI 2ME PGM", 925, state
        )

        assert changed is False, "nothing needed to change, so no force-set"
        # The snapshot must now reflect THIS run's own observation (925), overwriting the
        # stale leftover (3) -- never left empty/discarded.
        saved = state["10.0.0.1"][obs_phase2._TEST_LATENCY_STATE_KEY]
        assert saved["latency_ms"] == 925, (
            f"a fresh snapshot must ALWAYS be saved, even when nothing is forced -- "
            f"got {saved!r}"
        )
        # No SetInputSettings for the latency key -- genuinely nothing was forced.
        sets = [p for op, p in calls if op == "SetInputSettings"
                and "genlock_latency_ms_src" in p.get("inputSettings", {})]
        assert sets == [], f"already-at-target must not issue a redundant SetInputSettings: {sets}"

    def test_restore_puts_back_saved_latency(self, monkeypatch):
        rpc_response = {
            "GetInputSettings": {
                "inputSettings": {"genlock_latency_ms_src": 450},
            },
        }
        calls = _patch_capture(monkeypatch, rpc_response=rpc_response)
        state = {
            "10.0.0.1": {
                obs_phase2._TEST_LATENCY_STATE_KEY: {
                    "input": "NDI 2ME PGM",
                    "latency_ms": 450,
                    "render_delays": [],
                }
            }
        }
        stderr_lines = []
        monkeypatch.setattr(sys, "stderr",
                            type("FakeStderr", (), {"write": lambda s, msg: stderr_lines.append(msg)})())

        obs_phase2._restore_test_latency(None, "10.0.0.1", state)

        # SetInputSettings called for latency restore
        sets = [c for c in calls if c[0] == "SetInputSettings"]
        latency_sets = [p for _, p in sets
                        if p.get("inputName") == "NDI 2ME PGM"
                        and "genlock_latency_ms_src" in p.get("inputSettings", {})]
        assert len(latency_sets) == 1
        assert latency_sets[0]["inputSettings"]["genlock_latency_ms_src"] == 450, \
            "teardown MUST restore 450 (prod A/V-align, #358)"

        # state entry cleared so double-restore is impossible
        assert obs_phase2._TEST_LATENCY_STATE_KEY not in state["10.0.0.1"]

    def test_restore_noop_when_nothing_snapshotted(self, monkeypatch):
        calls = _patch_capture(monkeypatch)
        state = {"10.0.0.1": {"prev_scene": "Cam5"}}  # no _TEST_LATENCY_STATE_KEY
        obs_phase2._restore_test_latency(None, "10.0.0.1", state)
        assert [c for c in calls if c[0] == "SetInputSettings"] == []

    def test_restore_warns_loud_when_readback_differs_from_snapshot(self, monkeypatch):
        # Read-back returns 3ms, but snapshot saved 450ms → LOUD warn (#246 pattern)
        rpc_response = {
            "GetInputSettings": {
                "inputSettings": {"genlock_latency_ms_src": 3},  # wrong — should be 450
            },
        }
        calls = _patch_capture(monkeypatch, rpc_response=rpc_response)
        state = {
            "10.0.0.1": {
                obs_phase2._TEST_LATENCY_STATE_KEY: {
                    "input": "NDI 2ME PGM",
                    "latency_ms": 450,
                    "render_delays": [],
                }
            }
        }
        stderr_lines = []
        monkeypatch.setattr(sys, "stderr",
                            type("FakeStderr", (), {"write": lambda s, msg: stderr_lines.append(msg)})())

        obs_phase2._restore_test_latency(None, "10.0.0.1", state)

        warn_lines = [l for l in stderr_lines if "WARN" in l or "warn" in l.lower() or "mismatch" in l.lower() or "358" in l]
        assert warn_lines, (
            "restore must emit a LOUD warning when read-back latency ≠ snapshot "
            f"(stderr: {stderr_lines!r})"
        )

    # -----------------------------------------------------------------------
    # #691 belt-and-braces: calibrated-value cross-check (optional)
    # -----------------------------------------------------------------------

    def test_691_calibrated_mismatch_warns_loud_even_when_restore_matches_snapshot(
        self, monkeypatch
    ):
        # The restore itself is internally consistent (read-back == snapshot, 1000ms) --
        # but that snapshot was ALREADY wrong (e.g. a prior run's incomplete restore left
        # the box at 1000ms, and THIS run's own snapshot just captured that stuck value).
        # The snapshot-vs-restore check alone cannot see this; the calibrated cross-check
        # (925ms, the real av-sync-last.json value) must catch it.
        rpc_response = {
            "GetInputSettings": {
                "inputSettings": {"genlock_latency_ms_src": 1000},
            },
        }
        _patch_capture(monkeypatch, rpc_response=rpc_response)
        state = {
            "10.0.0.1": {
                obs_phase2._TEST_LATENCY_STATE_KEY: {
                    "input": "NDI 2ME PGM", "latency_ms": 1000, "render_delays": [],
                }
            }
        }
        stderr_lines = []
        monkeypatch.setattr(sys, "stderr",
                            type("FakeStderr", (), {"write": lambda s, msg: stderr_lines.append(msg)})())

        obs_phase2._restore_test_latency(
            None, "10.0.0.1", state, calibrated_latency_ms=925
        )

        warn_lines = [l for l in stderr_lines if "691" in l and "calibrat" in l.lower()]
        assert warn_lines, (
            f"a calibrated-value mismatch must emit a LOUD #691 warning even when the "
            f"restore matched its own (already-wrong) snapshot (stderr: {stderr_lines!r})"
        )

    def test_691_calibrated_match_emits_no_extra_warning(self, monkeypatch):
        rpc_response = {
            "GetInputSettings": {
                "inputSettings": {"genlock_latency_ms_src": 925},
            },
        }
        _patch_capture(monkeypatch, rpc_response=rpc_response)
        state = {
            "10.0.0.1": {
                obs_phase2._TEST_LATENCY_STATE_KEY: {
                    "input": "NDI 2ME PGM", "latency_ms": 925, "render_delays": [],
                }
            }
        }
        stderr_lines = []
        monkeypatch.setattr(sys, "stderr",
                            type("FakeStderr", (), {"write": lambda s, msg: stderr_lines.append(msg)})())

        obs_phase2._restore_test_latency(
            None, "10.0.0.1", state, calibrated_latency_ms=925
        )

        warn_lines = [l for l in stderr_lines if "691" in l and "calibrat" in l.lower()]
        assert warn_lines == [], (
            f"a matching calibrated value must NOT emit the #691 mismatch warning: "
            f"{stderr_lines!r}"
        )

    def test_691_calibrated_check_is_skipped_when_not_supplied(self, monkeypatch):
        # Default (None) — the common unattended-CI case, no operator-gathered value
        # available. Must not warn, must not error.
        rpc_response = {
            "GetInputSettings": {
                "inputSettings": {"genlock_latency_ms_src": 1000},  # would mismatch 925
            },
        }
        _patch_capture(monkeypatch, rpc_response=rpc_response)
        state = {
            "10.0.0.1": {
                obs_phase2._TEST_LATENCY_STATE_KEY: {
                    "input": "NDI 2ME PGM", "latency_ms": 1000, "render_delays": [],
                }
            }
        }
        stderr_lines = []
        monkeypatch.setattr(sys, "stderr",
                            type("FakeStderr", (), {"write": lambda s, msg: stderr_lines.append(msg)})())

        obs_phase2._restore_test_latency(None, "10.0.0.1", state)  # no calibrated_latency_ms

        warn_lines = [l for l in stderr_lines if "691" in l and "calibrat" in l.lower()]
        assert warn_lines == [], (
            f"the calibrated cross-check must be silently skipped when not supplied: "
            f"{stderr_lines!r}"
        )

    def test_restore_re_enables_gpu_delay_filters(self, monkeypatch):
        rpc_response = {
            "GetInputSettings": {
                "inputSettings": {"genlock_latency_ms_src": 450},
            },
        }
        calls = _patch_capture(monkeypatch, rpc_response=rpc_response)
        state = {
            "10.0.0.1": {
                obs_phase2._TEST_LATENCY_STATE_KEY: {
                    "input": "NDI 2ME PGM",
                    "latency_ms": 450,
                    "render_delays": [
                        {"filterName": "Render Delay", "was_enabled": True, "delay_ms": 500},
                    ],
                }
            }
        }
        monkeypatch.setattr(sys, "stderr",
                            type("FakeStderr", (), {"write": lambda s, msg: None})())

        obs_phase2._restore_test_latency(None, "10.0.0.1", state)

        filter_sets = [p for _, p in calls if _ == "SetSourceFilterEnabled"]
        assert any(
            p.get("sourceName") == "NDI 2ME PGM"
            and p.get("filterName") == "Render Delay"
            and p.get("filterEnabled") is True
            for p in filter_sets
        ), f"must re-enable 'Render Delay' filter, got: {filter_sets}"


# ---------------------------------------------------------------------------
# (f) CLI: --test-latency-ms parsed and forwarded
# ---------------------------------------------------------------------------

class TestCLITestLatencyMs:
    def test_prod_scene_accepts_test_latency_ms_arg(self, monkeypatch):
        captured = {}
        monkeypatch.setattr(obs_phase2, "prod_scene",
                            lambda a: captured.update(latency=getattr(a, "test_latency_ms", "MISSING")))
        monkeypatch.setattr(
            sys, "argv",
            ["obs_phase2.py", "prod-scene", "--host", "h", "--program-scene", "Cam 5",
             "--test-latency-ms", "1000"],
        )
        obs_phase2.main()
        assert captured.get("latency") == 1000, (
            f"--test-latency-ms 1000 must reach prod_scene as a.test_latency_ms=1000; "
            f"got {captured!r}"
        )

    def test_prod_scene_test_latency_ms_defaults_to_none_not_a_forced_1000(self, monkeypatch):
        # #691: the CLI default changed from a blind forced 1000 to `None` ("not
        # explicitly requested") — resolve_test_latency_ms derives the EFFECTIVE value
        # from the box's own current latency at call time instead (see that function's
        # own tests below). GENLOCK_TEST_LATENCY_MS still forces a literal value when set.
        captured = {}
        monkeypatch.setattr(obs_phase2, "prod_scene",
                            lambda a: captured.update(latency=getattr(a, "test_latency_ms", "MISSING")))
        monkeypatch.setattr(
            sys, "argv",
            ["obs_phase2.py", "prod-scene", "--host", "h", "--program-scene", "Cam 5"],
        )
        obs_phase2.main()
        assert captured.get("latency") is None, (
            f"unset --test-latency-ms must reach prod_scene as a.test_latency_ms=None "
            f"(#691 — auto-derived later from the box's current value); got {captured!r}"
        )

    def test_prod_scene_test_latency_ms_honors_explicit_env_override(self, monkeypatch):
        captured = {}
        monkeypatch.setattr(obs_phase2, "prod_scene",
                            lambda a: captured.update(latency=getattr(a, "test_latency_ms", "MISSING")))
        monkeypatch.setenv("GENLOCK_TEST_LATENCY_MS", "1200")
        monkeypatch.setattr(
            sys, "argv",
            ["obs_phase2.py", "prod-scene", "--host", "h", "--program-scene", "Cam 5"],
        )
        obs_phase2.main()
        assert captured.get("latency") == 1200, (
            f"an explicit GENLOCK_TEST_LATENCY_MS env override must still win verbatim; "
            f"got {captured!r}"
        )


# ---------------------------------------------------------------------------
# #691 — resolve_test_latency_ms: the EFFECTIVE test-latency decision
# ---------------------------------------------------------------------------

class TestResolveTestLatencyMs:
    def test_explicit_request_always_wins_verbatim(self):
        # An operator/supervisor override is never second-guessed, regardless of what
        # the box's current value happens to be.
        assert obs_phase2.resolve_test_latency_ms(1200, current_ms=925) == 1200
        assert obs_phase2.resolve_test_latency_ms(1200, current_ms=3) == 1200

    def test_unset_uses_current_value_when_already_above_floor(self):
        # The #691 live scenario: the box is calibrated at 925ms, well above the 500ms
        # floor -- use it AS-IS. Nothing is forced, so there is nothing to stomp.
        assert obs_phase2.resolve_test_latency_ms(None, current_ms=925) == 925

    def test_unset_uses_current_value_exactly_at_the_floor(self):
        assert (
            obs_phase2.resolve_test_latency_ms(
                None, current_ms=500, floor_ms=500
            )
            == 500
        )

    def test_unset_falls_back_to_1000_when_current_is_below_the_floor(self):
        # The box is at the prod A/V-align baseline (450ms) -- below the 500ms floor, so
        # nothing already exercises the #292 >450ms cap regression. Fall back to 1000.
        assert obs_phase2.resolve_test_latency_ms(None, current_ms=450) == 1000
        assert obs_phase2.resolve_test_latency_ms(None, current_ms=3) == 1000

    def test_custom_floor_and_fallback_are_honored(self):
        assert (
            obs_phase2.resolve_test_latency_ms(
                None, current_ms=600, floor_ms=700, fallback_ms=2000
            )
            == 2000
        )
        assert (
            obs_phase2.resolve_test_latency_ms(
                None, current_ms=800, floor_ms=700, fallback_ms=2000
            )
            == 800
        )

    def test_defaults_match_the_module_constants(self):
        assert obs_phase2.DEFAULT_TEST_LATENCY_CURRENT_FLOOR_MS == 500
        assert obs_phase2.DEFAULT_TEST_LATENCY_FALLBACK_MS == 1000
