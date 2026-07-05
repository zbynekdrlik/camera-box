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
    # The common case: no orphan. record() does the single pre-check, then StartRecord —
    # no StopRecord, no idle-poll. (Characterization: holds on both the old and new code.)
    calls = []

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        calls.append(rtype)
        if rtype == "GetRecordStatus":
            return {"outputActive": False, "outputTimecode": "00:00:00"}
        return {}

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2.time, "sleep", lambda *_a, **_k: None)

    obs_phase2.record(_start_args("h"))

    assert "StopRecord" not in calls, f"a clean box must NOT StopRecord: {calls}"
    assert calls.count("StartRecord") == 1, f"StartRecord must be issued once: {calls}"
    assert calls.count("GetRecordStatus") == 1, (
        f"a clean box needs only the single pre-check, no idle-poll: {calls}"
    )


def test_record_start_warns_loud_on_orphan(monkeypatch, capsys):
    # An orphan recording must be logged LOUD on stderr (WARN + host + the orphan's
    # timecode) so the silent abort cause is visible in the run log.
    status_seq = iter([
        {"outputActive": True, "outputTimecode": "05:27:13"},
        {"outputActive": False, "outputTimecode": "00:00:00"},
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
