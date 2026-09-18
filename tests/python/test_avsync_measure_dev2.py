"""#1331 -- unit tests for the PURE dev2-measurer helpers (scripts/lib/avsync-measure.sh) + the
retirement planner added to scripts/avsync-watchdog-install.sh + the dev2 installer's config
(scripts/avsync-dev2-install.sh, report-only sourcing).

Same `run_sourced` bash-sourcing convention as tests/rig_mode.rs / test_cam2_painter_wall_clock_1312.py.
The measurer moved OFF the stream box onto dev2, but the heartbeat CONTRACT + the ffmpeg encode
params are byte-identical to avsync-watchdog.ps1 -- these tests pin that.
"""

import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_HOME = str(pathlib.Path.home())
_MEASURE_LIB = _ROOT / "scripts" / "lib" / "avsync-measure.sh"
_WATCHDOG_INSTALL = _ROOT / "scripts" / "avsync-watchdog-install.sh"
_DEV2_INSTALL = _ROOT / "scripts" / "avsync-dev2-install.sh"


def _source(script: pathlib.Path, body: str, env_extra: dict | None = None) -> str:
    env = {"PATH": "/usr/bin:/bin", "HOME": _HOME, "SCRIPT": str(script)}
    if env_extra:
        env.update(env_extra)
    harness = 'set -uo pipefail\n. "$SCRIPT"\n' + body
    proc = subprocess.run(["bash", "-c", harness], env=env, capture_output=True, text=True)
    assert proc.returncode == 0, (
        f"harness exited {proc.returncode}\nstdout={proc.stdout!r}\nstderr={proc.stderr!r}"
    )
    return proc.stdout


def _measure(body: str, env_extra: dict | None = None) -> str:
    return _source(_MEASURE_LIB, body, env_extra)


# ── cadence (single source of truth for the timer) ───────────────────────────


def test_cadence_default_is_90():
    assert _measure("avsync_measure_cadence_s").strip() == "90"


def test_cadence_env_override():
    assert _measure("avsync_measure_cadence_s", {"AVSYNC_MEASURE_CADENCE_S": "45"}).strip() == "45"


# ── clip path / temp dir ─────────────────────────────────────────────────────


def test_clip_dir_falls_back_to_tmp_when_no_xdg():
    # XDG_RUNTIME_DIR unset -> /tmp (never $HOME; the clip is throwaway).
    assert _measure("avsync_measure_clip_dir").strip() == "/tmp/avsync-measure-dev2"


def test_clip_dir_falls_back_to_tmp_when_xdg_not_writable():
    out = _measure("avsync_measure_clip_dir", {"XDG_RUNTIME_DIR": "/nonexistent-xdg-9f3"})
    assert out.strip() == "/tmp/avsync-measure-dev2"


def test_clip_dir_uses_xdg_when_writable():
    body = (
        'd="$(mktemp -d)"\n'
        'XDG_RUNTIME_DIR="$d" avsync_measure_clip_dir\n'
    )
    out = _measure(body).strip()
    assert out.endswith("/avsync-measure-dev2")
    assert out != "/tmp/avsync-measure-dev2"


def test_clip_path_appends_clip_filename():
    assert _measure('avsync_measure_clip_path /some/dir').strip() == "/some/dir/live-clip.mp4"


# ── ffmpeg grab argv: SINGLE SOURCE OF TRUTH, byte-mirrors avsync-watchdog.ps1:99 ────────────────


def test_ffmpeg_grab_argv_has_the_exact_encode_params():
    out = _measure('avsync_measure_ffmpeg_grab_argv "rtmp://x/y" "/tmp/c.mp4"')
    lines = out.splitlines()
    assert "rtmp://x/y" in lines
    assert "/tmp/c.mp4" in lines
    assert "scale=1280:-2,fps=25" in lines
    assert "libx264" in lines
    assert "veryfast" in lines
    assert "26" in lines           # -crf 26
    assert "aac" in lines
    assert "16000" in lines        # -ar 16000
    assert "1" in lines            # -ac 1
    assert "35" in lines           # -t 35 (default clip secs)


def test_ffmpeg_grab_argv_secs_override():
    out = _measure('avsync_measure_ffmpeg_grab_argv "rtmp://x/y" "/tmp/c.mp4" 20')
    assert "20" in out.splitlines()


def test_ffmpeg_grab_argv_has_input_rw_timeout_before_i():
    # #1331 review nit #4: a per-read INPUT timeout so a dead relay yields an honest no-signal
    # heartbeat (ffmpeg exits non-zero) instead of a SIGTERM'd pass with NO heartbeat. Must be an
    # INPUT option -> before `-i`.
    lines = _measure('avsync_measure_ffmpeg_grab_argv "rtmp://x/y" "/tmp/c.mp4"').splitlines()
    assert "-rw_timeout" in lines
    assert "15000000" in lines
    assert lines.index("-rw_timeout") < lines.index("-i"), "rw_timeout must be an INPUT option (before -i)"


def test_ffmpeg_grab_argv_rw_timeout_override():
    out = _measure(
        'avsync_measure_ffmpeg_grab_argv "rtmp://x/y" "/tmp/c.mp4"',
        {"AVSYNC_MEASURE_RW_TIMEOUT_US": "9000000"},
    )
    assert "9000000" in out.splitlines()


def test_ffmpeg_grab_argv_url_and_clip_are_single_args():
    # A URL/clip with a space must survive as ONE arg (mapfile -t line == the whole value).
    out = _measure('avsync_measure_ffmpeg_grab_argv "rtmp://a b/c" "/tmp/my clip.mp4"')
    lines = out.splitlines()
    assert "rtmp://a b/c" in lines
    assert "/tmp/my clip.mp4" in lines


# ── freshness reason (fail-CLOSED, mirrors the ps1 gate) ─────────────────────


def test_freshness_reason_empty_when_ok():
    assert _measure('avsync_measure_freshness_reason 0 "OK"') == ""


def test_freshness_reason_tolerates_trailing_cr_on_ok():
    assert _measure("avsync_measure_freshness_reason 0 \"$(printf 'OK\\r')\"") == ""


def test_freshness_reason_extracts_no_signal_reason():
    out = _measure('avsync_measure_freshness_reason 10 "NO-SIGNAL: clip too small (5 B < 200000 B)"')
    assert out == "clip too small (5 B < 200000 B)"


def test_freshness_reason_nonzero_rc_with_ok_text_still_not_fresh():
    # rc!=0 must NEVER be treated as fresh even if the text happens to say OK.
    out = _measure('avsync_measure_freshness_reason 1 "OK"')
    assert out.startswith("freshness gate unavailable (rc=1):")


def test_freshness_reason_fallback_on_garbage():
    out = _measure('avsync_measure_freshness_reason 127 "python3: command not found"')
    assert out.startswith("freshness gate unavailable (rc=127):")
    assert "command not found" in out


# ── status line (byte-identical to avsync-watchdog.ps1 Write-Heartbeat) ──────


def test_status_line_measured():
    out = _measure('avsync_measure_status_line "" "-5.4" "[x] AV offset +0 fr (+0 ms) conf 8 :: A/V sync OK (offset 0 ms)"')
    assert out == "measured: db=-5.4 [x] AV offset +0 fr (+0 ms) conf 8 :: A/V sync OK (offset 0 ms)"


def test_status_line_no_signal():
    out = _measure('avsync_measure_status_line "grab failed: ffmpeg rc=-5 (relay/stream down)" "" ""')
    assert out == "no-signal: grab failed: ffmpeg rc=-5 (relay/stream down)"


def test_heartbeat_record_is_epoch_tab_status():
    out = _measure('avsync_measure_heartbeat_record 1700000000 "measured: db=-5.4 x"')
    assert out == "1700000000\tmeasured: db=-5.4 x"


# ── retirement planner in avsync-watchdog-install.sh ─────────────────────────


def test_disable_stream_tasks_cmds_lists_all_three():
    out = _source(_WATCHDOG_INSTALL, "avsync_disable_stream_tasks_cmds")
    lines = [l for l in out.splitlines() if l.strip()]
    assert len(lines) == 3
    for task in ("avsync-watchdog", "avsync-keepalive", "avsync-vlc-monitor"):
        assert any(f"/TN {task} /DISABLE" in l for l in lines), f"missing DISABLE for {task}"
    # never DELETE -- the tasks can be re-enabled if the dev2 measurer is unavailable.
    assert "/Delete" not in out


def test_retire_mode_prints_disable_plan():
    proc = subprocess.run(
        ["bash", str(_WATCHDOG_INSTALL), "--retire"],
        env={"PATH": "/usr/bin:/bin", "HOME": _HOME}, capture_output=True, text=True,
    )
    assert proc.returncode == 0, proc.stderr
    assert "schtasks /Change /TN avsync-watchdog /DISABLE" in proc.stdout
    assert "dev2" in proc.stdout


# ── dev2 installer config (report-only sourcing) ─────────────────────────────


def test_dev2_install_help_exits_zero():
    proc = subprocess.run(
        ["bash", str(_DEV2_INSTALL), "--help"],
        env={"PATH": "/usr/bin:/bin", "HOME": _HOME}, capture_output=True, text=True,
    )
    assert proc.returncode == 0
    assert "--check" in proc.stdout and "--install" in proc.stdout


def test_dev2_install_no_arg_is_usage_error():
    proc = subprocess.run(
        ["bash", str(_DEV2_INSTALL)],
        env={"PATH": "/usr/bin:/bin", "HOME": _HOME}, capture_output=True, text=True,
    )
    assert proc.returncode == 2


def test_dev2_install_pip_deps_are_the_two_syncnet_only_ones():
    # torch/cv2/scipy/numpy come from the system cu128 stack (--system-site-packages); only the two
    # SyncNet-specific deps are pip-installed. Guard against silently pip-pulling a heavy torch.
    out = _source(_DEV2_INSTALL, 'printf "%s\\n" "${PIP_DEPS[@]}"')
    deps = set(l.strip() for l in out.splitlines() if l.strip())
    assert deps == {"scenedetect", "python_speech_features"}, deps


# ── #1331 review 🔴: the dev2 measurer must NOT emit Discord (delivery is the dev1 watchdogs' job) ─
_MEASURER = _ROOT / "scripts" / "avsync-measure-dev2.sh"


def test_measurer_neutralizes_av_sync_measure_discord_delivery():
    # av_sync_measure.py's DEFAULT delivery (deliver_alert -> airuleset.py notify, on |offset| >=
    # threshold) would make dev2 PING DISCORD ITSELF -- a duplicate of the dev1 watchdog alert.
    # The measurer must point AIRULESET_NOTIFY at a neutralizing (empty) script, defaulting to
    # /dev/null, and must NEVER pass --webhook (the raw-webhook path).
    src = _MEASURER.read_text()
    assert 'MEASURE_NOTIFY_BIN="${AVSYNC_MEASURE_NOTIFY_BIN:-/dev/null}"' in src, \
        "MEASURE_NOTIFY_BIN must default to a neutralizing /dev/null"
    assert 'AIRULESET_NOTIFY="$MEASURE_NOTIFY_BIN"' in src, \
        "the av_sync_measure.py call must run with AIRULESET_NOTIFY pointed at the neutralizing bin"
    assert "--webhook" not in src, "the dev2 measurer must never pass --webhook (raw Discord POST)"


def test_measurer_notify_bin_default_is_dev_null():
    # Prove the DEFAULT resolves to /dev/null (a genuine no-op), not the real airuleset.py.
    out = _source(_MEASURER, 'printf "%s" "$MEASURE_NOTIFY_BIN"')
    assert out == "/dev/null"


def test_neutralized_notify_path_is_a_real_noop():
    # `python3 /dev/null notify --body X --dedup-key Y` must exit 0 and print nothing -- the exact
    # invocation av_sync_measure.py's notify_airuleset builds when AIRULESET_NOTIFY=/dev/null.
    proc = subprocess.run(
        ["python3", "/dev/null", "notify", "--body", "x", "--dedup-key", "k"],
        env={"PATH": "/usr/bin:/bin", "HOME": _HOME}, capture_output=True, text=True,
    )
    assert proc.returncode == 0
    assert proc.stdout == "" and proc.stderr == ""


# ── #1331 verified-A/V report: the per-day durable log (builders + the dev2 append) ──────────────


def test_log_path_defaults_to_home_avsync_measurements_day():
    out = _measure('avsync_measure_log_path "" 2026-09-17')
    assert out == f"{_HOME}/avsync/measurements-2026-09-17.tsv"


def test_log_path_explicit_base():
    out = _measure('avsync_measure_log_path /srv/box 2026-01-02')
    assert out == "/srv/box/avsync/measurements-2026-01-02.tsv"


def test_log_line_is_byte_identical_to_the_heartbeat_record():
    line = _measure('avsync_measure_log_line 1700000000 "measured: db=-5.4 x"')
    record = _measure('avsync_measure_heartbeat_record 1700000000 "measured: db=-5.4 x"')
    assert line == record == "1700000000\tmeasured: db=-5.4 x"


def test_append_day_log_creates_the_day_file_and_appends_rows(tmp_path):
    body = (
        'avsync_measure_append_day_log 1700000000 "measured: db=-5.0 x"\n'
        'avsync_measure_append_day_log 1700000090 "no-signal: y"\n'
        'day=$(date -d @1700000000 +%Y-%m-%d)\n'
        'cat "$HOME/avsync/measurements-$day.tsv"\n'
    )
    out = _source(_MEASURER, body, {"HOME": str(tmp_path)})
    assert out.splitlines() == [
        "1700000000\tmeasured: db=-5.0 x",
        "1700000090\tno-signal: y",
    ]


def test_append_day_log_uses_the_local_day_from_the_pass_epoch(tmp_path):
    body = (
        'avsync_measure_append_day_log 1700000000 "measured: db=-5.0 x"\n'
        'ls "$HOME/avsync"\n'
    )
    out = _source(_MEASURER, body, {"HOME": str(tmp_path)})
    import subprocess as _sp

    day = _sp.run(
        ["date", "-d", "@1700000000", "+%Y-%m-%d"], capture_output=True, text=True
    ).stdout.strip()
    assert out.strip() == f"measurements-{day}.tsv"


def test_main_appends_the_day_log_after_writing_the_heartbeat():
    src = _MEASURER.read_text()
    assert "avsync_measure_append_day_log" in src
    hb = src.index('write_heartbeat "$record"')
    ap = src.index("avsync_measure_append_day_log", hb)
    assert ap > hb, "the day-log append must come after write_heartbeat in main()"
