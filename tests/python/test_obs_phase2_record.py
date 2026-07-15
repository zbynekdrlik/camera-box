"""#105 recording-based E2E: unit tests for the obs_phase2 `record` subcommand wiring.

The recording-based full-path harness (scripts/recording-e2e.sh) drives OBS program
recording via `obs_phase2.py record --action start|stop|status`. These tests pin the
argument-parsing + dispatch wiring (NOT the live WebSocket calls, which need a real
OBS) so a future edit that drops the `record` subcommand or its `--action` choices
fails loudly in CI.
"""
import argparse
import importlib.util
import pathlib
import sys

import pytest


class _FakeWS:
    """Minimal stand-in for the obs-websocket connection: record() calls ws.close()
    in its finally, so the fake just needs a no-op close()."""

    def close(self):
        pass

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_rec", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_rec"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


def test_record_function_exists():
    # The recording-based harness depends on this entrypoint existing.
    assert callable(obs_phase2.record)


def test_record_subcommand_parses_each_action(monkeypatch):
    # `obs_phase2.py record --action {start,stop,status,guard} --host H` must parse and
    # dispatch to record() — never to setup/teardown. Patch record() to capture args.
    captured = {}

    def fake_record(a):
        captured["host"] = a.host
        captured["action"] = a.action

    monkeypatch.setattr(obs_phase2, "record", fake_record)
    for action in ("start", "stop", "status", "guard"):
        captured.clear()
        monkeypatch.setattr(
            sys, "argv",
            ["obs_phase2.py", "record", "--host", "10.77.9.202", "--action", action],
        )
        obs_phase2.main()
        assert captured == {"host": "10.77.9.202", "action": action}


def test_record_rejects_unknown_action(monkeypatch):
    # An invalid --action must be rejected by argparse (choices guard), never silently
    # passed through to a no-op.
    monkeypatch.setattr(
        sys, "argv",
        ["obs_phase2.py", "record", "--host", "h", "--action", "bogus"],
    )
    with pytest.raises(SystemExit):
        obs_phase2.main()


def test_record_action_is_required(monkeypatch):
    # Omitting --action must fail (a recording call with no action is a harness bug).
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "record", "--host", "h"])
    with pytest.raises(SystemExit):
        obs_phase2.main()


# ---------------------------------------------------------------------------
# #163: `prod-scene` — route OBS program to a CERTIFIED PRODUCTION scene and record
# IT, instead of pointing the colliding `phase2-probe-src` ndi_source at a source-name
# the always-on prod input already holds (which records pure BLACK — see #163). These
# tests pin the new entrypoint + its non-black fail-fast self-check (pure luma helper),
# without any live WebSocket.
# ---------------------------------------------------------------------------

def test_prod_scene_function_exists():
    # The recording harness routes program via this new entrypoint (records the prod
    # scene program, never the colliding probe input).
    assert callable(obs_phase2.prod_scene)


def test_prod_scene_subcommand_parses_and_dispatches(monkeypatch):
    # `obs_phase2.py prod-scene --host H --program-scene "Cam 5"` must parse and
    # dispatch to prod_scene() — never to setup/record/teardown.
    captured = {}

    def fake_prod_scene(a):
        captured["host"] = a.host
        captured["program_scene"] = a.program_scene

    monkeypatch.setattr(obs_phase2, "prod_scene", fake_prod_scene)
    monkeypatch.setattr(
        sys, "argv",
        ["obs_phase2.py", "prod-scene", "--host", "10.77.9.202",
         "--program-scene", "Cam 5"],
    )
    obs_phase2.main()
    assert captured == {"host": "10.77.9.202", "program_scene": "Cam 5"}


def test_prod_scene_requires_program_scene(monkeypatch):
    # The program scene name is mandatory — routing program with no target scene is a
    # harness bug, never a silent no-op.
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "prod-scene", "--host", "h"])
    with pytest.raises(SystemExit):
        obs_phase2.main()


def test_is_black_luma_helper_flags_black_and_passes_nonblack():
    # The fail-fast non-black self-check (#163 candidate fix 3): a recorded-black probe
    # ingest must be caught BEFORE StartRecord wastes a full run. The pure decision
    # helper treats an all-zero (peak luma 0) frame as black and a frame with any signal
    # (non-zero peak) as non-black — the decision is on the PEAK only.
    assert obs_phase2._luma_is_black(luma_max=0) is True
    assert obs_phase2._luma_is_black(luma_max=255) is False
    # A frame with a real peak (e.g. the live 'Cam 5' read peak=255) is NON-black even
    # when its mean is low (a mostly-dark but signal-bearing camera frame).
    assert obs_phase2._luma_is_black(luma_max=1) is False


def test_blackcheck_verdict_polls_until_deadline_before_failing():
    # REGRESSION (#111 deploy): the non-black self-check used a SINGLE read 2 s after the
    # scene switch. A cold DistroAV NDI receiver (high genlock_preload, re-establishing
    # from idle) needs longer than 2 s to fill its FIFO and render the first non-black
    # frame — so the single-shot read saw BLACK and aborted a fully-healthy run. The fix
    # makes the check POLL: a black read BEFORE the deadline means WAIT (keep polling),
    # not FAIL; only black AT/AFTER the deadline is a real FAIL.
    #
    # The pure verdict over (luma_max, elapsed_s, timeout_s):
    #   - non-black peak           -> "OK"   (proceed immediately, however early)
    #   - black, elapsed < timeout -> "WAIT" (receiver may still be filling; keep polling)
    #   - black, elapsed >= timeout-> "BLACK"(genuinely dead source; abort)
    #   - luma unreadable (None)   -> "WAIT" while in budget, else "UNKNOWN" (never silent OK)
    assert obs_phase2._blackcheck_verdict(luma_max=255, elapsed_s=0.0, timeout_s=15.0) == "OK"
    assert obs_phase2._blackcheck_verdict(luma_max=1, elapsed_s=14.9, timeout_s=15.0) == "OK"
    # Black early in the budget = WAIT (this is the exact case the single-shot 2 s read
    # mis-classified as FAIL and aborted the #111 run).
    assert obs_phase2._blackcheck_verdict(luma_max=0, elapsed_s=2.0, timeout_s=15.0) == "WAIT"
    assert obs_phase2._blackcheck_verdict(luma_max=0, elapsed_s=14.0, timeout_s=15.0) == "WAIT"
    # Black at/after the deadline = genuine BLACK -> abort.
    assert obs_phase2._blackcheck_verdict(luma_max=0, elapsed_s=15.0, timeout_s=15.0) == "BLACK"
    assert obs_phase2._blackcheck_verdict(luma_max=0, elapsed_s=20.0, timeout_s=15.0) == "BLACK"
    # Unreadable luma: WAIT while in budget (retry the screenshot), UNKNOWN past it (never
    # a silent OK — the caller warns + proceeds, recording-verdict still catches all-black).
    assert obs_phase2._blackcheck_verdict(luma_max=None, elapsed_s=3.0, timeout_s=15.0) == "WAIT"
    assert obs_phase2._blackcheck_verdict(luma_max=None, elapsed_s=15.0, timeout_s=15.0) == "UNKNOWN"


SCENES = ["Cam 5", "Cam 1", "test 2", "REC-STRIH-TMP", "POST"]


def test_restore_target_real_prod_scene_is_faithful_even_when_it_was_program():
    # strih 'Cam 5' is a REAL prod scene. If it was ALREADY the live program when the run
    # started, the faithful restore keeps the box on 'Cam 5' — never bumps it onto an
    # arbitrary other scene (the smoke-run regression that left strih on 'test 2').
    assert obs_phase2._restore_target(
        prev="Cam 5", target="Cam 5", ephemeral=False, scenes=SCENES
    ) == "Cam 5"
    # And if a different scene was program, that scene is restored.
    assert obs_phase2._restore_target(
        prev="POST", target="Cam 5", ephemeral=False, scenes=SCENES
    ) == "POST"


def test_restore_target_ephemeral_scene_is_never_restored_to_itself():
    # The stream temp scene (built via --ensure-source) is EPHEMERAL: restoring program to
    # it would strand the throwaway record scene as live program. If it was program (a
    # crash), recover the saved prior; else fall back to any OTHER existing scene.
    assert obs_phase2._restore_target(
        prev="REC-STRIH-TMP", target="REC-STRIH-TMP", ephemeral=True, scenes=SCENES,
        saved_prev="POST",
    ) == "POST"
    # No saved prior → fall back to some other (never the ephemeral target).
    got = obs_phase2._restore_target(
        prev="REC-STRIH-TMP", target="REC-STRIH-TMP", ephemeral=True, scenes=SCENES,
    )
    assert got != "REC-STRIH-TMP" and got in SCENES
    # A normal prior program (the usual case) is restored verbatim.
    assert obs_phase2._restore_target(
        prev="POST", target="REC-STRIH-TMP", ephemeral=True, scenes=SCENES,
    ) == "POST"


# ---------------------------------------------------------------------------
# #355: the `record --action start` orphan guard must POLL the record output to
# idle (finalized) before StartRecord — a flat sleep(1.0) is too short to finalize
# a large MP4 (the live 24.5 GB orphan that 500'd StartRecord on the stream box),
# so StartRecord ran while the output was STILL active and got `{code:500}` → the
# whole capstone run aborted. These tests drive record() with a fake WS where
# GetRecordStatus is the state machine; no live OBS.
# ---------------------------------------------------------------------------


def _start_args(host="10.77.9.204"):
    return argparse.Namespace(host=host, password="", action="start")


def test_record_start_polls_record_status_until_orphan_finalizes(monkeypatch):
    # An orphan recording is active; after StopRecord it keeps finalizing for a couple
    # of polls, then idles. record() MUST keep polling GetRecordStatus until idle and
    # issue StartRecord only AFTER idle — never after a single fixed sleep.
    calls = []
    status_seq = iter([
        {"outputActive": True, "outputTimecode": "05:27:13"},   # initial pre-check
        {"outputActive": True, "outputTimecode": "05:27:14"},   # poll 1: still finalizing
        {"outputActive": True, "outputTimecode": "05:27:15"},   # poll 2: still finalizing
        {"outputActive": False, "outputTimecode": "00:00:00"},  # poll 3: idle (finalized)
        # #627: two more samples for the post-StartRecord liveness check — active + growing
        # bytes, i.e. a genuinely healthy start.
        {"outputActive": True, "outputBytes": 1000},
        {"outputActive": True, "outputBytes": 5000},
    ])
    grs_count = {"n": 0}
    start_marker = {"grs_at_start": None}

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        calls.append(rtype)
        if rtype == "GetRecordStatus":
            grs_count["n"] += 1
            return next(status_seq)
        if rtype == "StartRecord":
            start_marker["grs_at_start"] = grs_count["n"]
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2.time, "sleep", lambda *_a, **_k: None)

    obs_phase2.record(_start_args())

    assert calls.count("StopRecord") == 1, f"orphan must be stopped exactly once: {calls}"
    assert calls.count("StartRecord") == 1, f"StartRecord must be issued once: {calls}"
    # POLLING proof: GetRecordStatus issued MORE than once (initial + ≥2 polls). The old
    # flat sleep(1.0) path checks status ONCE then sleeps blindly — this is the RED assertion.
    assert grs_count["n"] >= 3, (
        "record start must POLL GetRecordStatus until the orphan finalizes "
        f"(outputActive=False) before StartRecord — a flat sleep checks it once. calls={calls}"
    )
    # StartRecord only AFTER the poll loop observed idle (the 4th GetRecordStatus).
    assert start_marker["grs_at_start"] == 4, (
        "StartRecord must be issued only AFTER GetRecordStatus reports idle — never while the "
        f"output is still finalizing. grs_at_start={start_marker['grs_at_start']}"
    )


def test_record_start_fails_loud_if_orphan_never_finalizes(monkeypatch):
    # If the orphan never finalizes within the bounded timeout, record() MUST FAIL LOUD
    # (SystemExit) instead of issuing a doomed StartRecord on a still-active output.
    calls = []
    clock = {"t": 0.0}

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        calls.append(rtype)
        if rtype == "GetRecordStatus":
            return {"outputActive": True, "outputTimecode": "05:27:13"}  # NEVER idles
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    # Fake clock: sleep advances it so the bounded deadline is reached deterministically.
    monkeypatch.setattr(obs_phase2.time, "monotonic", lambda: clock["t"])
    monkeypatch.setattr(obs_phase2.time, "sleep", lambda s: clock.__setitem__("t", clock["t"] + s))

    with pytest.raises(SystemExit):
        obs_phase2.record(_start_args())

    assert "StartRecord" not in calls, (
        "StartRecord must NOT be issued when the orphan never finalizes — the harness must FAIL "
        f"LOUD rather than start a doomed StartRecord on a still-active output. calls={calls}"
    )


def test_record_start_clean_box_starts_without_stop_or_poll(monkeypatch):
    # The common case: no orphan. record() does the single pre-check, then StartRecord,
    # then the #627 post-start liveness check (still no StopRecord, no orphan idle-poll).
    calls = []
    started = {"yes": False}
    byte_seq = iter([1000, 5000])  # #627 liveness samples: active + growing bytes

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        calls.append(rtype)
        if rtype == "StartRecord":
            started["yes"] = True
        if rtype == "GetRecordStatus":
            if not started["yes"]:
                return {"outputActive": False, "outputTimecode": "00:00:00"}
            return {"outputActive": True, "outputBytes": next(byte_seq)}
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2.time, "sleep", lambda *_a, **_k: None)

    obs_phase2.record(_start_args("h"))

    assert "StopRecord" not in calls, f"a clean box must NOT StopRecord: {calls}"
    assert calls.count("StartRecord") == 1, f"StartRecord must be issued once: {calls}"
    # 1 pre-check + 2 #627 post-start liveness samples — no orphan idle-poll.
    assert calls.count("GetRecordStatus") == 3, (
        f"a clean box needs the single pre-check plus the #627 post-start liveness-check "
        f"samples, no idle-poll: {calls}"
    )


def test_record_start_warns_loud_on_orphan(monkeypatch, capsys):
    # An orphan recording must be logged LOUD on stderr (WARN + host + the orphan's
    # timecode) so the silent abort cause is visible in the run log.
    status_seq = iter([
        {"outputActive": True, "outputTimecode": "05:27:13"},
        {"outputActive": False, "outputTimecode": "00:00:00"},
        # #627: two more samples for the post-StartRecord liveness check.
        {"outputActive": True, "outputBytes": 1000},
        {"outputActive": True, "outputBytes": 5000},
    ])

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        if rtype == "GetRecordStatus":
            return next(status_seq)
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2.time, "sleep", lambda *_a, **_k: None)

    obs_phase2.record(_start_args("10.77.9.204"))

    err = capsys.readouterr().err
    assert "WARN" in err, f"an orphan recording must be logged LOUD (WARN): {err!r}"
    assert "orphan" in err.lower(), f"the WARN must name it an orphan: {err!r}"
    assert "10.77.9.204" in err, f"the WARN must name the box: {err!r}"
    assert "05:27:13" in err, f"the WARN must include the orphan's timecode: {err!r}"


# ---------------------------------------------------------------------------
# #524: `record --action guard` — the pre-event stray-recording guard. strih's runaway
# 265.9 GiB recording (~11h to full) filled the disk mid-event, and a SECOND stray
# recording (18.57 GiB) started the SAME event day — both were only caught + stopped
# manually. `rig-mode.sh event` now calls this guard on strih + stream BEFORE anything
# else: GetRecordStatus -> if active, StopRecord (keeps the file) + a loud WARN naming
# the host + the stray file, so recurrence is caught automatically instead of by luck.
# ---------------------------------------------------------------------------


def _guard_args(host="10.77.9.202"):
    return argparse.Namespace(host=host, password="", action="guard")


def test_record_guard_stops_active_recording_and_warns_with_path(monkeypatch, capsys):
    calls = []

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        calls.append(rtype)
        if rtype == "GetRecordStatus":
            return {"outputActive": True, "outputTimecode": "11:24:07"}
        if rtype == "StopRecord":
            return {"outputPath": "C:\\Recordings\\strih-2026-07-05.mkv"}
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.record(_guard_args("10.77.9.202"))

    assert calls == ["GetRecordStatus", "StopRecord"], (
        f"guard must check status then StopRecord exactly once when active: {calls}"
    )
    err = capsys.readouterr().err
    assert "WARN" in err, f"a stray recording must be logged LOUD (WARN): {err!r}"
    assert "10.77.9.202" in err, f"the WARN must name the box: {err!r}"
    assert "C:\\Recordings\\strih-2026-07-05.mkv" in err, (
        f"the WARN must name the stray file that was left recording: {err!r}"
    )
    assert "11:24:07" in err, f"the WARN must include how long it had been recording: {err!r}"


def test_record_guard_no_stray_does_not_stop_and_prints_clean(monkeypatch, capsys):
    calls = []

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        calls.append(rtype)
        if rtype == "GetRecordStatus":
            return {"outputActive": False, "outputTimecode": "00:00:00"}
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.record(_guard_args("10.77.9.204"))

    assert "StopRecord" not in calls, f"a clean box must NOT StopRecord: {calls}"
    err = capsys.readouterr().err
    assert "WARN" not in err, f"a clean box must not warn: {err!r}"


# ---------------------------------------------------------------------------
# #627: `record --action start`'s post-StartRecord liveness check. A stream-box StartRecord
# call reported success ("recording STARTED" logged) but GetRecordStatus ~1800s later (after
# StopRecord) showed outputActive=false, outputBytes=0 — no file was ever written, discovered
# only at fetch time after the whole run duration was burned. This is a fail-fast DETECTION
# (poll a few seconds after StartRecord, abort loud if not genuinely live), NOT a root-cause
# fix — the cause of that one silent StartRecord failure remains unproven (see #627).
# ---------------------------------------------------------------------------


def test_record_liveness_verdict_passes_when_active_and_bytes_growing():
    # The healthy case: active every sample, bytes strictly increasing.
    is_live, reason = obs_phase2._record_liveness_verdict(
        active_flags=[True, True], byte_counts=[1000, 5000]
    )
    assert is_live is True
    assert reason == ""


def test_record_liveness_verdict_fails_on_zero_bytes_the_exact_627_symptom():
    # The EXACT #627 symptom: outputActive stayed True (per the WS status) but outputBytes
    # never moved off zero for the whole window — no file was ever written.
    is_live, reason = obs_phase2._record_liveness_verdict(
        active_flags=[True, True], byte_counts=[0, 0]
    )
    assert is_live is False
    assert "outputBytes" in reason
    assert "0" in reason


def test_record_liveness_verdict_fails_when_output_goes_inactive():
    # The output died during the liveness window itself — active must hold for EVERY sample.
    is_live, reason = obs_phase2._record_liveness_verdict(
        active_flags=[True, False], byte_counts=[1000, 1000]
    )
    assert is_live is False
    assert "outputActive" in reason


def test_record_liveness_verdict_fails_when_bytes_do_not_grow():
    # Bytes are non-zero but STALLED (no growth across the window) — a recording that wrote
    # a little then froze immediately. A bytes>0-only check would miss this.
    is_live, reason = obs_phase2._record_liveness_verdict(
        active_flags=[True, True], byte_counts=[1000, 1000]
    )
    assert is_live is False
    assert "grow" in reason.lower()


def test_record_liveness_verdict_fails_on_no_samples():
    # Never silently pass on an empty/unreadable measurement window.
    is_live, reason = obs_phase2._record_liveness_verdict(active_flags=[], byte_counts=[])
    assert is_live is False
    assert reason


def test_record_liveness_verdict_treats_missing_outputbytes_as_dead():
    # An older obs-ws build (or an unreadable response) with no outputBytes key at all: the
    # caller defaults it to -1 (never a silent "live" pass on unreadable data).
    is_live, reason = obs_phase2._record_liveness_verdict(
        active_flags=[True, True], byte_counts=[-1, -1]
    )
    assert is_live is False


# ---------------------------------------------------------------------------
# #710: a COLD OBS restart's FIRST post-restart StartRecord can false-abort — the encoder's
# NVENC/CUDA cold-init took slightly longer than one liveness poll to flip outputActive to
# True, even though the recording genuinely started fine within the SAME liveness window's
# NEXT sample (exact live shape: active=[False, True] bytes=[0, 623840] on imag-nb, found
# verifying #709). A LEADING run of outputActive=False must be tolerated IFF every sample
# from the first True onward stays True — never once the output has gone True and then False
# again (that is a genuine "started then died", #627's own original symptom).
# ---------------------------------------------------------------------------


def test_record_liveness_verdict_tolerates_cold_start_false_then_true_with_growing_bytes():
    # The exact live #710 repro shape: first sample still warming up (inactive, 0 bytes),
    # second sample genuinely live with real growing bytes.
    is_live, reason = obs_phase2._record_liveness_verdict(
        active_flags=[False, True], byte_counts=[0, 623840]
    )
    assert is_live is True
    assert reason == ""


def test_record_liveness_verdict_fails_when_never_goes_true():
    # The genuine #627/#709 shape: outputActive never flips True at all during the whole
    # window — must stay a hard DEAD, cold-start tolerance never masks this.
    is_live, reason = obs_phase2._record_liveness_verdict(
        active_flags=[False, False], byte_counts=[0, 0]
    )
    assert is_live is False
    assert "outputActive" in reason


def test_record_liveness_verdict_fails_when_it_dies_after_a_cold_start():
    # A cold start that DOES eventually go live, then dies again — must NOT be tolerated as
    # a cold start; a True->False transition anywhere is still a genuine death.
    is_live, reason = obs_phase2._record_liveness_verdict(
        active_flags=[False, True, False], byte_counts=[0, 1000, 1000]
    )
    assert is_live is False
    assert "outputActive" in reason


def test_record_liveness_verdict_cold_start_still_enforces_byte_growth():
    # A tolerated cold start (False then True) must still fail if the output never actually
    # writes any bytes once active — the #627 zero-bytes symptom, cold-start tolerance must
    # not weaken this check.
    is_live, reason = obs_phase2._record_liveness_verdict(
        active_flags=[False, True], byte_counts=[0, 0]
    )
    assert is_live is False
    assert "outputBytes" in reason


def test_record_start_aborts_loud_on_dead_on_arrival_recording(monkeypatch):
    # REGRESSION (#627): StartRecord succeeds (obs-ws reports it started), but the output
    # writes ZERO bytes the whole time — record() must FAIL LOUD (SystemExit) within the
    # short liveness window instead of letting the caller sleep through the entire run and
    # discover a 0-byte file only at fetch time.
    calls = []
    started = {"yes": False}

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        calls.append(rtype)
        if rtype == "StartRecord":
            started["yes"] = True
        if rtype == "GetRecordStatus":
            if not started["yes"]:
                # No orphan — the pre-check must be clean so StartRecord actually fires;
                # otherwise this would hit the #355 orphan path instead of the #627 one.
                return {"outputActive": False, "outputTimecode": "00:00:00"}
            # Dead-on-arrival (active=true, 0 bytes) for both post-start liveness samples —
            # the exact #627 symptom.
            return {"outputActive": True, "outputBytes": 0}
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2.time, "sleep", lambda *_a, **_k: None)

    with pytest.raises(SystemExit) as exc_info:
        obs_phase2.record(_start_args("10.77.9.204"))

    assert calls.count("StartRecord") == 1, (
        f"StartRecord must still be issued once — the check runs AFTER it, not instead: {calls}"
    )
    msg = str(exc_info.value)
    assert "10.77.9.204" in msg, f"the abort must name the host: {msg!r}"
    assert "#627" in msg, f"the abort must reference #627: {msg!r}"


def test_record_start_passes_liveness_check_on_a_healthy_recording(monkeypatch, capsys):
    # Characterization of the happy path: StartRecord succeeds, the output is active and
    # bytes grow across the liveness window — record() returns normally (no SystemExit), and
    # logs the liveness OK line for the run log.
    started = {"yes": False}
    byte_seq = iter([2000, 6000])

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        if rtype == "StartRecord":
            started["yes"] = True
        if rtype == "GetRecordStatus":
            if not started["yes"]:
                return {"outputActive": False, "outputTimecode": "00:00:00"}
            return {"outputActive": True, "outputBytes": next(byte_seq)}
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2.time, "sleep", lambda *_a, **_k: None)

    obs_phase2.record(_start_args("10.77.9.202"))  # must not raise

    err = capsys.readouterr().err
    assert "liveness OK" in err, f"a healthy start must log the liveness OK line: {err!r}"


def test_record_start_passes_liveness_check_on_a_cold_restart_first_start(monkeypatch, capsys):
    # REGRESSION (#710): the FIRST StartRecord right after a fresh OBS process restart —
    # NVENC/CUDA cold-init makes outputActive stay False for the liveness check's first
    # sample, then flip True (with growing bytes) on the next. record() must NOT abort; this
    # is the exact live imag-nb repro (active=[False, True] bytes=[0, 623840]).
    started = {"yes": False}
    status_seq = iter(
        [
            {"outputActive": False, "outputBytes": 0},  # liveness sample 1: still warming up
            {"outputActive": True, "outputBytes": 623840},  # liveness sample 2: genuinely live
        ]
    )

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        if rtype == "StartRecord":
            started["yes"] = True
        if rtype == "GetRecordStatus":
            if not started["yes"]:
                # Pre-check orphan probe — clean box, no orphan.
                return {"outputActive": False, "outputTimecode": "00:00:00"}
            return next(status_seq)
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2.time, "sleep", lambda *_a, **_k: None)

    obs_phase2.record(_start_args("10.77.9.182"))  # must not raise

    err = capsys.readouterr().err
    assert "liveness OK" in err, f"a tolerated cold start must still log liveness OK: {err!r}"


def test_record_start_grace_tolerates_slow_nvenc_byte_counter(monkeypatch, capsys):
    # REGRESSION (#767 deploy, 2026-07-15, run 29417639968): imag's NVENC cold-init takes
    # ~3s from StartRecord to the muxer actually writing, and outputBytes lags a further
    # beat -- the fixed 2x2s liveness window sampled active=[False, True] bytes=[0, 0] and
    # ABORTED a recording that was in fact healthy (the file grew to 296MB until cleanup
    # stopped it). When the LAST sample is active with bytes still 0, record() must keep
    # polling within a bounded grace window and pass once bytes appear -- never abort a
    # genuinely-live recording on a slow byte counter.
    started = {"yes": False}
    # post-start sequence: warming up (False,0) -> active but 0 bytes (True,0) -> grace
    # polls see bytes appear and grow.
    seq = iter([
        {"outputActive": False, "outputBytes": 0},
        {"outputActive": True, "outputBytes": 0},
        {"outputActive": True, "outputBytes": 0},
        {"outputActive": True, "outputBytes": 512000},
    ])

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        if rtype == "StartRecord":
            started["yes"] = True
        if rtype == "GetRecordStatus":
            if not started["yes"]:
                return {"outputActive": False, "outputTimecode": "00:00:00"}
            return next(seq, {"outputActive": True, "outputBytes": 1024000})
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2.time, "sleep", lambda *_a, **_k: None)

    obs_phase2.record(_start_args("10.77.9.182"))  # must NOT raise

    err = capsys.readouterr().err
    assert "liveness OK" in err
    assert "grace" in err, (
        f"the log must surface that the byte-grace fired (greppable per run): {err!r}"
    )


def test_record_start_grace_still_aborts_when_bytes_never_appear(monkeypatch):
    # The #627 protection must SURVIVE the grace: an output that stays active with 0 bytes
    # past the whole grace budget is still dead-on-arrival -> SystemExit.
    started = {"yes": False}

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        if rtype == "StartRecord":
            started["yes"] = True
        if rtype == "GetRecordStatus":
            if not started["yes"]:
                return {"outputActive": False, "outputTimecode": "00:00:00"}
            return {"outputActive": True, "outputBytes": 0}
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2.time, "sleep", lambda *_a, **_k: None)

    with pytest.raises(SystemExit) as exc_info:
        obs_phase2.record(_start_args("10.77.9.182"))
    assert "#627" in str(exc_info.value)
