"""#1207 — av_sync_measure.py alert delivery: default routes through airuleset notify with a stable
per-kind --dedup-key; the raw --webhook path stays an EXPLICIT opt-in override with a simple
in-process per-kind throttle. Delivery layer only — the #1206 doctrine (analyze-not-ping, stable
per-incident dedup key) extended to this one remaining raw-webhook emitter.

Root cause the fix addresses: `scripts/av_sync_measure.py` was the ONE alert emitter in the repo
that still POSTed to a RAW Discord webhook (`notify_discord()` → urllib) with no dedup and entirely
outside `airuleset.py notify` — so the #1206 `--dedup-key` sweep never covered it. The fix adds a
single `deliver_alert(args, kind, text)` seam that both call-sites (`run_outer_loop`,
`one_measurement`) route through:
  * DEFAULT (no --webhook) → `airuleset.py notify --body <text> --dedup-key av-sync-measure-<kind>`.
  * EXPLICIT --webhook URL → raw webhook (as today) + a per-kind cooldown so a sustained state does
    not re-POST every --loop round.

These are Tier-0 unit tests (no real syncnet_python / ffmpeg / obs-websocket / airuleset — the
subprocess + notify_discord seams are monkeypatched). Detection/measurement is out of scope and
proven unchanged: the correction-event trigger and the `|offset| >= threshold` check are NOT touched
by these tests — only WHERE the alert is delivered.
"""
import pathlib
import sys
import types

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_sync_measure  # noqa: E402


@pytest.fixture(autouse=True)
def _clear_module_state():
    """The raw-webhook per-kind throttle (and the outer-loop guard cache) are module-level state;
    clear both around every test so tests never contaminate each other or the sibling
    test_av_sync_outer_loop_apply.py (which shares the same imported module in one pytest process).
    Defensive `hasattr` so the RED run (before `_WEBHOOK_LAST_SENT` exists) fails on the test bodies'
    own assertions, not here."""
    for name in ("_WEBHOOK_LAST_SENT", "_OUTER_LOOP_GUARDS"):
        if hasattr(av_sync_measure, name):
            getattr(av_sync_measure, name).clear()
    yield
    for name in ("_WEBHOOK_LAST_SENT", "_OUTER_LOOP_GUARDS"):
        if hasattr(av_sync_measure, name):
            getattr(av_sync_measure, name).clear()


def _args(webhook=None):
    return types.SimpleNamespace(webhook=webhook)


def _capture_subprocess(monkeypatch):
    calls = []
    monkeypatch.setattr(
        av_sync_measure.subprocess, "run",
        lambda cmd, **kw: calls.append(list(cmd)) or types.SimpleNamespace(returncode=0, stdout="", stderr=""),
    )
    return calls


# ---------------------------------------------------------------------------
# DEFAULT path (no --webhook) → airuleset notify with a stable per-kind --dedup-key
# ---------------------------------------------------------------------------

class TestDefaultRoutesThroughAiruleset:
    def test_verdict_default_uses_airuleset_notify_with_dedup_key(self, monkeypatch):
        calls = _capture_subprocess(monkeypatch)
        posted = []
        monkeypatch.setattr(av_sync_measure, "notify_discord", lambda w, t: posted.append((w, t)))

        av_sync_measure.deliver_alert(_args(webhook=None), "verdict", "🎯 AV-sync watchdog: X")

        assert posted == [], "the default path must NOT use the raw webhook urllib POST"
        assert len(calls) == 1, f"the default path must route through airuleset notify, got {calls}"
        cmd = calls[0]
        assert "notify" in cmd and "--body" in cmd, cmd
        assert "🎯 AV-sync watchdog: X" in cmd
        assert "--dedup-key" in cmd, cmd
        assert cmd[cmd.index("--dedup-key") + 1] == "av-sync-measure-verdict"

    def test_asrc_default_uses_its_own_dedup_key(self, monkeypatch):
        calls = _capture_subprocess(monkeypatch)
        av_sync_measure.deliver_alert(_args(webhook=None), "asrc", "🎚️ ASRC outer-loop: Y")
        cmd = calls[0]
        assert cmd[cmd.index("--dedup-key") + 1] == "av-sync-measure-asrc"

    def test_airuleset_failure_is_best_effort_never_raises(self, monkeypatch):
        def _boom(cmd, **kw):
            raise OSError("airuleset missing")
        monkeypatch.setattr(av_sync_measure.subprocess, "run", _boom)
        # must not raise — a missing/failing airuleset never aborts a measurement
        av_sync_measure.deliver_alert(_args(webhook=None), "verdict", "🎯 X")


# ---------------------------------------------------------------------------
# EXPLICIT --webhook override → raw webhook (kept) + per-kind throttle
# ---------------------------------------------------------------------------

class TestWebhookOverride:
    def test_webhook_set_posts_raw_and_skips_airuleset(self, monkeypatch):
        posted = []
        calls = _capture_subprocess(monkeypatch)
        monkeypatch.setattr(av_sync_measure, "notify_discord", lambda w, t: posted.append((w, t)))

        av_sync_measure.deliver_alert(_args(webhook="https://d/w"), "verdict", "🎯 X")

        assert posted == [("https://d/w", "🎯 X")], "explicit --webhook must keep the raw webhook path"
        assert calls == [], "the webhook override must NOT also fire airuleset notify"

    def test_webhook_throttles_repeat_of_same_kind(self, monkeypatch):
        posted = []
        monkeypatch.setattr(av_sync_measure, "notify_discord", lambda w, t: posted.append(t))
        a = _args(webhook="https://d/w")

        av_sync_measure.deliver_alert(a, "verdict", "🎯 first")
        av_sync_measure.deliver_alert(a, "verdict", "🎯 second (still desynced)")

        assert posted == ["🎯 first"], (
            "a repeat of the SAME kind within the throttle window must be suppressed "
            "(the raw webhook has no dedup of its own)"
        )

    def test_webhook_different_kinds_are_not_cross_throttled(self, monkeypatch):
        posted = []
        monkeypatch.setattr(av_sync_measure, "notify_discord", lambda w, t: posted.append(t))
        a = _args(webhook="https://d/w")

        av_sync_measure.deliver_alert(a, "verdict", "🎯 v")
        av_sync_measure.deliver_alert(a, "asrc", "🎚️ a")

        assert posted == ["🎯 v", "🎚️ a"], "the throttle is PER KIND — a different kind still delivers"

    def test_webhook_resends_after_the_cooldown_elapses(self, monkeypatch):
        posted = []
        monkeypatch.setattr(av_sync_measure, "notify_discord", lambda w, t: posted.append(t))
        a = _args(webhook="https://d/w")

        av_sync_measure.deliver_alert(a, "verdict", "🎯 first")
        # simulate the cooldown having elapsed by ageing the recorded send time
        av_sync_measure._WEBHOOK_LAST_SENT["verdict"] -= av_sync_measure.WEBHOOK_THROTTLE_S + 1.0
        av_sync_measure.deliver_alert(a, "verdict", "🎯 later")

        assert posted == ["🎯 first", "🎯 later"]


# ---------------------------------------------------------------------------
# Call-site wiring — both alert sites route through deliver_alert (detection unchanged)
# ---------------------------------------------------------------------------

def _measure_args(tmp_path, *, webhook=None, threshold_ms=60, outer_loop=False):
    media = tmp_path / "clip.mp4"
    media.write_bytes(b"fake")
    return types.SimpleNamespace(
        media=str(media), grab=None, secs=20, webhook=webhook, threshold_ms=threshold_ms,
        calibration_log=None, outer_loop=outer_loop,
    )


class TestCallSitesRouteThroughDeliverAlert:
    def test_one_measurement_suprathreshold_default_delivers_verdict(self, monkeypatch, tmp_path):
        monkeypatch.setattr(av_sync_measure, "measure", lambda repo, media, workdir: [(3, 8.5, None)])
        seen = []
        monkeypatch.setattr(av_sync_measure, "deliver_alert", lambda args, kind, text: seen.append((kind, text)))

        rc = av_sync_measure.one_measurement(_measure_args(tmp_path, webhook=None), tmp_path)

        assert rc == 0
        assert len(seen) == 1, f"a suprathreshold measurement must deliver exactly one alert, got {seen}"
        assert seen[0][0] == "verdict"
        assert "🎯 AV-sync watchdog" in seen[0][1]

    def test_one_measurement_subthreshold_delivers_nothing(self, monkeypatch, tmp_path):
        # 1 frame * 40ms = 40ms < 60ms threshold — detection must still gate delivery.
        monkeypatch.setattr(av_sync_measure, "measure", lambda repo, media, workdir: [(1, 8.5, None)])
        seen = []
        monkeypatch.setattr(av_sync_measure, "deliver_alert", lambda args, kind, text: seen.append((kind, text)))

        av_sync_measure.one_measurement(_measure_args(tmp_path, webhook=None), tmp_path)

        assert seen == [], "a sub-threshold offset must not deliver any alert (detection unchanged)"

    def test_run_outer_loop_correction_routes_asrc_through_deliver_alert(self, monkeypatch, tmp_path):
        from av_sync_outer_loop_guard import WINDOW_N

        monkeypatch.setattr(av_sync_measure, "_conn", lambda h, p: types.SimpleNamespace(close=lambda: None))
        monkeypatch.setattr(av_sync_measure, "apply_outer_bias", lambda ws, src, cur, new: new)
        seen = []
        monkeypatch.setattr(av_sync_measure, "deliver_alert", lambda args, kind, text: seen.append((kind, text)))

        args = types.SimpleNamespace(
            webhook=None, outer_loop=True, outer_loop_state=str(tmp_path / "state.json"),
            outer_loop_source="mbc", ws_host="h", ws_password="",
        )
        for _ in range(WINDOW_N):
            av_sync_measure.run_outer_loop(args, 60.0)

        assert len(seen) == 1, f"a sustained correction must deliver exactly one asrc alert, got {seen}"
        assert seen[0][0] == "asrc"
        assert "🎚️ ASRC outer-loop" in seen[0][1]
