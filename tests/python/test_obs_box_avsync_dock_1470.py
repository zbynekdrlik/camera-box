"""issue 1470 -- the camera-box A/V-sync dock on the Linux OBS boxes.

Owner 23.10.2026: the stream OBS on stream-snv lost the A/V-sync dock when the stream box went from
Windows to Linux, because only windows-genlock.yml built vendor/av-sync-dock. These tests pin:
  * linux-genlock.yml's strih job configures + builds the dock against the genlocked SDK after
    obs-vban, proves the layout seed against the real Qt, and stages the .so + its data next to
    obs-vban.so (the existing bundle -> setup-strih step 4 install path, no second one); the imag-parity
    bundle stays without it; a vendor/av-sync-dock change re-runs the build;
  * scripts/lib/obs-box-avsync-dock.sh under the callers' `set -euo pipefail`: the step-16g layout seed
    (stream role only, never while OBS runs, WARN-only) and verify item 47's three rows;
  * the setup / verify wiring and their 1000-line budgets.
"""

import os
import re
import shutil
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
LIB = SCRIPTS / "lib" / "obs-box-avsync-dock.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
WF = REPO / ".github" / "workflows" / "linux-genlock.yml"
FIX = Path(__file__).resolve().parent / "fixtures" / "obs_dock_state_1470"
DOCK = "net.nagater.obs-audio-video-sync-dock..main"
ME = os.environ.get("USER") or "nobody"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_LX_DANTESYNC_ROLE", "STRIH_BOXES_DIR", "OBS_FLEET")


def _bash(body, env=None, box="stream-pp", args=()):
    """BODY under the callers' strict mode with the box loaded and the provision lib sourced."""
    script = 'set -euo pipefail\n. "$1"\nstrih_box_load "$2"\n. "$3"\nshift 3\n' + body
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    r = subprocess.run(["bash", "-c", script, "harness", str(FACTS), box, str(PROVISION), *args],
                       capture_output=True, text=True, env=e, timeout=120)
    assert "unbound variable" not in r.stderr, r.stderr
    return r


def _obs_running(tmp_path, running):
    p = tmp_path / ("obs-up" if running else "obs-down")
    p.write_text("#!/bin/bash\nexit %d\n" % (0 if running else 1))
    p.chmod(0o755)
    # the ONE "is OBS running" probe of the provisioning libs (strih_operator_obs_running) and its seam
    return {"STRIH_OPERATOR_OBS_RUNNING_CMD": str(p)}


def _ini(tmp_path, box="stream-pp"):
    p = tmp_path / "user.ini"
    shutil.copy(FIX / f"{box}.user.ini", p)
    return p


# --- the CI build ---------------------------------------------------------------------------------

def _strih_job(wf):
    start = wf.index("  linux-genlock-build-strih:")
    return wf[start:]


def test_the_strih_job_builds_the_dock_after_obs_vban_and_before_staging():
    job = _strih_job(WF.read_text())
    at = lambda s: job.index(s)
    vban = at("      - name: Build obs-vban (issue 1439)")
    conf = at("      - name: Configure av-sync-dock against the genlocked OBS SDK (issue 1470)")
    build = at("      - name: Build av-sync-dock (issue 1470)")
    proof = at("      - name: Prove the A/V-sync dock layout seed against the real Qt (issue 1470)")
    stage = at("      - name: Stage artifact")
    assert vban < conf < build < proof < stage
    step = job[conf:build]
    assert "working-directory: vendor/av-sync-dock" in step
    assert '-DCMAKE_PREFIX_PATH="$GITHUB_WORKSPACE/obs-sdk"' in step and "-DQT_VERSION=6" in step
    assert "cmake --build build_ubuntu --parallel" in job[build:proof]
    assert "run: bash tests/qt/run_dock_state_restore_1470.sh" in job[proof:stage]


def test_the_stage_step_puts_the_dock_and_its_data_next_to_obs_vban():
    job = _strih_job(WF.read_text())
    stage = job[job.index("      - name: Stage artifact"):job.index("      - name: Record runtime packages")]
    find = "dock_so=$(find vendor/av-sync-dock/build_ubuntu -name 'obs-audio-video-sync-dock.so' -not -path '*/CMakeFiles/*' | head -1)"
    assert find in stage
    assert 'cp "$dock_so" "$plugin_lib_dir/"' in stage
    assert stage.index('cp "$vban_so" "$plugin_lib_dir/"') < stage.index('cp "$dock_so" "$plugin_lib_dir/"')
    assert 'cp -a vendor/av-sync-dock/data/. "$data_root/obs-audio-video-sync-dock/"' in stage
    assert "nm -D --defined-only \"$dock_so\"" in stage and "obs_module_load" in stage
    # the stage fails loud on a missing dock: never a bundle silently without it
    assert 'if [ -z "$dock_so" ]; then echo "ERROR: obs-audio-video-sync-dock.so not found' in stage
    assert "exit 1" in stage[stage.index('if [ -z "$dock_so" ]'):]


def test_the_imag_bundle_and_the_compile_check_do_not_build_the_dock():
    wf = WF.read_text()
    before = wf[:wf.index("  linux-genlock-build-strih:")]
    assert "av-sync-dock" not in before.split("\njobs:\n", 1)[1]


def test_a_dock_or_seed_change_reruns_the_linux_build():
    wf = WF.read_text()
    paths = wf[wf.index("    paths:\n"):wf.index("\npermissions:")]
    for p in ("'vendor/av-sync-dock/**'", "'scripts/obs_dock_state.py'", "'tests/qt/**'",
              "'tests/python/fixtures/obs_dock_state_1470/**'"):
        assert f"      - {p}\n" in paths, p


def test_the_linux_workflow_still_carries_no_pwsh_guard_copy():
    # The dock's source anchors run as Rust tests in ci.yml (the twins of the windows pwsh steps);
    # this workflow proves the gcc compile + link, never a third copy of the text guards.
    assert "shell: pwsh" not in WF.read_text()
    twins = sorted(p.name for p in (REPO / "tests").glob("av_sync_dock_*.rs"))
    assert "av_sync_dock_qr_patch_guard.rs" in twins and len(twins) >= 5


def test_the_qt_runner_fails_loud_without_its_tools_and_proves_four_cases():
    runner = (REPO / "tests" / "qt" / "run_dock_state_restore_1470.sh").read_text()
    assert "set -euo pipefail" in runner.splitlines()[2]
    assert 'command -v "$tool" >/dev/null 2>&1 || { echo "FAIL: ${tool} is not installed' in runner
    assert "pkg-config --exists Qt6Widgets || { echo \"FAIL:" in runner
    for want in ('"${DOCK} visible=0 floating=1 area=2"', '[ "$got" = seeded ]', '[ "$got" = present ]',
                 '[ "$got" = default-hidden ]', '[ "$got" = redocked ]', 'DOCKED="${DOCK} visible=1 floating=0 area=2"'):
        assert want in runner, want
    harness = (REPO / "tests" / "qt" / "dock_state_restore_1470.cpp").read_text()
    assert "win.restoreState(QByteArray::fromBase64(" in harness
    assert "d->setVisible(false);" in harness and "d->setFloating(true);" in harness


# --- the lib: setup step 16g -----------------------------------------------------------------------

def test_the_seed_runs_on_the_stream_role_and_docks_the_dock(tmp_path):
    ini = _ini(tmp_path)
    r = _bash('obs_box_avsync_dock_seed "$1" "$2"', env=_obs_running(tmp_path, False), args=(str(ini), ME))
    assert r.returncode == 0, r.stderr
    assert "A/V-sync dock seeded visible + docked right" in r.stdout
    r = subprocess.run(["python3", str(SCRIPTS / "obs_dock_state.py"), "check", "--ini", str(ini), "--dock", DOCK],
                       capture_output=True, text=True)
    assert r.stdout.strip() == "visible"


def test_the_seed_keeps_the_operators_layout(tmp_path):
    ini = _ini(tmp_path, "stream-snv")
    snap = ini.read_bytes()
    r = _bash('obs_box_avsync_dock_seed "$1" "$2"', env=_obs_running(tmp_path, False), args=(str(ini), ME),
              box="stream-snv")
    assert r.returncode == 0 and "already places it" in r.stdout
    assert ini.read_bytes() == snap


def test_the_seed_never_writes_while_obs_runs(tmp_path):
    ini = _ini(tmp_path)
    snap = ini.read_bytes()
    r = _bash('obs_box_avsync_dock_seed "$1" "$2"', env=_obs_running(tmp_path, True), args=(str(ini), ME))
    assert r.returncode == 0 and "WARN: OBS is running" in r.stdout
    assert ini.read_bytes() == snap


def test_the_strih_role_skips_the_seed(tmp_path):
    ini = _ini(tmp_path)
    snap = ini.read_bytes()
    r = _bash('obs_box_avsync_dock_seed "$1" "$2"', env=_obs_running(tmp_path, False), args=(str(ini), ME),
              box="strih-snv")
    assert r.returncode == 0 and "SKIP -- the A/V-sync dock reads the measurement audio on the stream box" in r.stdout
    assert ini.read_bytes() == snap


def test_an_unreadable_user_ini_is_a_warn_and_untouched(tmp_path):
    ini = tmp_path / "user.ini"
    ini.write_text("[BasicWindow]\nDockState=AAAA\n")
    r = _bash('obs_box_avsync_dock_seed "$1" "$2"', env=_obs_running(tmp_path, False), args=(str(ini), ME))
    assert r.returncode == 0 and "WARN: the A/V-sync dock could not be seeded" in r.stdout
    assert ini.read_text() == "[BasicWindow]\nDockState=AAAA\n"


MARK = "AvsyncDockSeeded=true"


def _marked(ini):
    lines = ini.read_text().splitlines()
    return "[CameraBox]" in lines and MARK in lines[lines.index("[CameraBox]") + 1:]


def _floated_and_closed(tmp_path):
    """A user.ini holding the dock hidden + floating in the right area: what OBS saves both when it ran
    with the plugin before any seed AND when the operator floated the dock and closed it."""
    import base64
    import importlib.util
    spec = importlib.util.spec_from_file_location("ods", SCRIPTS / "obs_dock_state.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    ini = _ini(tmp_path)
    text = ini.read_text()
    line = [l for l in text.splitlines() if l.startswith("DockState=")][0]
    blob = bytearray(m.seed(base64.b64decode(line.split("=", 1)[1]), DOCK))
    item = [i for i in m.parse(bytes(blob))["items"] if i["name"] == DOCK][0]
    blob[item["flags_offset"]] = 0x02
    ini.write_text(text.replace(line, "DockState=" + base64.b64encode(bytes(blob)).decode()))
    return ini


def test_the_first_seed_marks_user_ini(tmp_path):
    ini = _ini(tmp_path)
    r = _bash('obs_box_avsync_dock_seed "$1" "$2"', env=_obs_running(tmp_path, False), args=(str(ini), ME))
    assert r.returncode == 0 and "seeded" in r.stdout and _marked(ini)
    # an operator layout already holding the dock is marked too (stream-snv)
    d2 = tmp_path / "snv"
    d2.mkdir()
    ini2 = _ini(d2, "stream-snv")
    r = _bash('obs_box_avsync_dock_seed "$1" "$2"', env=_obs_running(tmp_path, False), args=(str(ini2), ME),
              box="stream-snv")
    assert "already places it" in r.stdout and _marked(ini2)
    # no side file next to user.ini (round 2: the marker lives and dies with user.ini)
    assert sorted(x.name for x in tmp_path.iterdir() if x.is_file()) == ["user.ini"]


def test_before_the_first_seed_the_obs_default_is_redocked(tmp_path):
    ini = _floated_and_closed(tmp_path)
    r = _bash('obs_box_avsync_dock_seed "$1" "$2"', env=_obs_running(tmp_path, False), args=(str(ini), ME))
    assert "A/V-sync dock redocked visible + docked right" in r.stdout, r.stdout
    assert _marked(ini)


def test_after_the_first_seed_a_floated_and_closed_dock_is_the_operators(tmp_path):
    # Review finding 2: once seeded, hidden + floating is the operator floating the dock and closing it;
    # every later setup run keeps it and verify only NOTEs it.
    ini = _floated_and_closed(tmp_path)
    ini.write_text(ini.read_text() + "\n[CameraBox]\n" + MARK + "\n")
    snap = ini.read_bytes()
    r = _bash('obs_box_avsync_dock_seed "$1" "$2"', env=_obs_running(tmp_path, False), args=(str(ini), ME))
    assert "already places it" in r.stdout and ini.read_bytes() == snap
    lib, share = _prefix(tmp_path)
    row = _rows(tmp_path, LOG_WITH, ini, lib, share)[2]
    assert row.startswith("NOTE|(avsync-dock-layout) the operator closed the A/V-sync dock"), row
    # a restored user.ini without the marker: OBS's default again, a FAIL until setup re-docks it
    ini.write_text(snap.decode().replace("\n[CameraBox]\n" + MARK + "\n", ""))
    row = _rows(tmp_path, LOG_WITH, ini, lib, share)[2]
    assert row.startswith("FAIL|(avsync-dock-layout) ") and "hidden + floating default" in row, row


def test_a_user_ini_without_a_saved_layout_is_a_warn(tmp_path):
    ini = tmp_path / "user.ini"
    ini.write_text("[BasicWindow]\nSaveProjectors=true\n")
    r = _bash('obs_box_avsync_dock_seed "$1" "$2"', env=_obs_running(tmp_path, False), args=(str(ini), ME))
    assert r.returncode == 0 and "has no saved OBS dock layout yet" in r.stdout


# --- the lib: verify item 47 -----------------------------------------------------------------------

LOG_WITH = """02:35:33.121:   Loaded Modules:
02:35:33.121:     vlc-video.so
02:35:33.121:     obs-vban.so
02:35:33.121:     obs-audio-video-sync-dock.so
02:35:33.121:     distroav.so
02:35:33.121: ---------------------------------
"""
LOG_WITHOUT = LOG_WITH.replace("02:35:33.121:     obs-audio-video-sync-dock.so\n", "")


def _prefix(tmp_path, so=True, locale=True):
    lib = tmp_path / "lib"
    share = tmp_path / "share"
    (lib / "obs-plugins").mkdir(parents=True)
    loc = share / "obs" / "obs-plugins" / "obs-audio-video-sync-dock" / "locale"
    loc.mkdir(parents=True)
    if so:
        (lib / "obs-plugins" / "obs-audio-video-sync-dock.so").write_bytes(b"\x7fELF")
    if locale:
        (loc / "en-US.ini").write_text("SyncTestDock.Title=\"Audio Video Sync\"\n")
    return lib, share


def _rows(tmp_path, log_text, ini, lib, share, box="stream-pp"):
    log = tmp_path / "obs.txt"
    if log_text is None:
        log_arg = ""
    else:
        log.write_text(log_text)
        log_arg = str(log)
    r = _bash('obs_box_avsync_dock_grade_rows "$1" "$2" "$3" "$4"', args=(str(lib), str(share), log_arg, str(ini)),
              box=box)
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


def test_the_strih_role_grades_the_plugin_but_not_the_layout(tmp_path):
    # Review finding 1: the strih role installs the same bundle, so OBS there loads the dock too; its
    # plugin rows are graded on every role, only the layout (the stream operator's dock) is not.
    lib, share = _prefix(tmp_path)
    ini = _ini(tmp_path, "stream-pp")
    for box in ("strih-snv", "strih-pp"):
        rows = _rows(tmp_path, LOG_WITH, ini, lib, share, box=box)
        assert rows[0].startswith("OK|(avsync-dock) ") and rows[1].startswith("OK|(avsync-dock-loaded) "), rows
        assert rows[2].startswith("NOTE|(avsync-dock-layout) not graded: the A/V-sync dock reads the measurement audio"), rows
    lib2, share2 = _prefix(tmp_path / "missing", so=False)
    rows = _rows(tmp_path, LOG_WITHOUT, ini, lib2, share2, box="strih-snv")
    assert rows[0].startswith("FAIL|(avsync-dock) ") and rows[1].startswith("FAIL|(avsync-dock-loaded) "), rows


def test_a_healthy_stream_box_reads_three_ok_rows(tmp_path):
    lib, share = _prefix(tmp_path)
    rows = _rows(tmp_path, LOG_WITH, _ini(tmp_path, "stream-snv"), lib, share)
    assert [r.split("|")[0] for r in rows] == ["OK", "OK", "OK"], rows
    assert rows[0].startswith("OK|(avsync-dock) ") and rows[1].startswith("OK|(avsync-dock-loaded) ")
    assert rows[2].startswith("OK|(avsync-dock-layout) ")


@pytest.mark.parametrize("so,locale", [(False, True), (True, False), (False, False)])
def test_a_missing_plugin_or_locale_fails_the_installed_row(tmp_path, so, locale):
    lib, share = _prefix(tmp_path, so, locale)
    rows = _rows(tmp_path, LOG_WITH, _ini(tmp_path, "stream-snv"), lib, share)
    assert rows[0].startswith("FAIL|(avsync-dock) the A/V-sync dock plugin is not installed"), rows
    assert "setup-strih.sh step 4" in rows[0]


def test_a_log_without_the_module_fails_the_loaded_row(tmp_path):
    lib, share = _prefix(tmp_path)
    rows = _rows(tmp_path, LOG_WITHOUT, _ini(tmp_path, "stream-snv"), lib, share)
    assert rows[1].startswith("FAIL|(avsync-dock-loaded) ") and "restart strih-obs.service" in rows[1]
    # a module that failed to load reads the same: the row names where to look
    assert "Failed to load" in rows[1]


@pytest.mark.parametrize("text", [None, "02:35:32.747: [distroav] plugin loaded\n"])
def test_no_log_or_no_module_list_is_a_note(tmp_path, text):
    lib, share = _prefix(tmp_path)
    rows = _rows(tmp_path, text, _ini(tmp_path, "stream-snv"), lib, share)
    assert rows[1].startswith("NOTE|(avsync-dock-loaded) "), rows


def test_the_loaded_verdict_is_drain_safe_on_a_large_log_under_pipefail(tmp_path):
    big = tmp_path / "big.txt"
    with open(big, "w") as fh:
        fh.write(LOG_WITH)
        fh.write(("x" * 200 + "\n") * 20000)
    r = _bash('set -o pipefail; cat "$1" | obs_box_avsync_dock_loaded_verdict; echo "rc=$?"', args=(str(big),))
    assert r.stdout.splitlines() == ["loaded", "rc=0"], r.stdout


def test_the_layout_row_reads_each_state(tmp_path):
    lib, share = _prefix(tmp_path)
    assert _rows(tmp_path, LOG_WITH, _ini(tmp_path, "stream-pp"), lib, share)[2].startswith(
        "FAIL|(avsync-dock-layout) the saved OBS layout") and "has no A/V-sync dock" in \
        _rows(tmp_path, LOG_WITH, _ini(tmp_path, "stream-pp"), lib, share)[2]
    ini = tmp_path / "plain.ini"
    ini.write_text("[BasicWindow]\nSaveProjectors=true\n")
    assert _rows(tmp_path, LOG_WITH, ini, lib, share)[2].startswith("FAIL|(avsync-dock-layout) OBS has saved no dock layout")
    ini.write_text("[BasicWindow]\nDockState=AAAA\n")
    assert _rows(tmp_path, LOG_WITH, ini, lib, share)[2].startswith("FAIL|(avsync-dock-layout) the saved OBS layout in")


def test_the_report_maps_rows_to_the_callers_verdicts_and_counts_them(tmp_path):
    lib, share = _prefix(tmp_path, so=False)
    body = ('ok() { echo "PASS $1"; }; bad() { echo "FAIL $1"; }; note() { echo "NOTE $1"; }\n'
            'rc=0; obs_box_avsync_dock_grade_report "$1" "$2" "" "$3" || rc=$?; echo "rc=$rc"\n')
    r = _bash(body, args=(str(lib), str(share), str(_ini(tmp_path, "stream-snv"))))
    out = r.stdout.splitlines()
    assert out[0].startswith("FAIL (avsync-dock) ") and out[1].startswith("NOTE (avsync-dock-loaded) ")
    assert out[2].startswith("PASS (avsync-dock-layout) ") and out[3] == "rc=0"


@pytest.mark.parametrize("box,runs", [("stream-pp", True), ("stream-snv", True), ("strih-snv", False), ("strih-pp", False)])
def test_the_step_gate_is_the_stream_role(box, runs):
    r = _bash('if WHY="$(obs_box_step_applies avsync-dock)"; then echo RUN; else echo "SKIP $WHY"; fi', box=box)
    assert r.stdout.startswith("RUN" if runs else "SKIP"), r.stdout


# --- the wiring --------------------------------------------------------------------------------------

def test_setup_seeds_the_dock_in_step_16g_after_the_operator_ini():
    s = SETUP.read_text()
    body = s[s.index('step "16g" '):s.index('\nstep "16h" ')]
    call = 'obs_box_avsync_dock_seed "${OBS_CFG}/user.ini" "$DESKTOP_USER"'
    assert s.count(call) == 1 and call in body
    assert body.index("strih_operator_install ") < body.index(call)
    assert len(s.splitlines()) <= 1000


def test_verify_grades_item_47_on_one_line_after_item_46():
    v = VERIFY.read_text()
    j = v.index("# 47) issue 1470")
    line = v[v.rindex("\n", 0, j) + 1:v.index("\n", j)]
    # every role (review finding 1): the grader gates only its layout row by the role
    assert line.startswith("obs_box_avsync_dock_grade_report ")
    assert "obs_box_step_applies" not in line
    assert '"${LOG:-}" "${USER_HOME}/.config/obs-studio/user.ini"' in line
    assert '|| bad "(avsync-dock) the grader did not print all of its rows"' in line
    assert v.index("# 46) issue 1394") < j < v.index("\n# 16) NIC")
    # item 47 reads the newest log item 2 already resolved
    assert v.index('LOG="$(newest_log)"') < j
    assert len(v.splitlines()) <= 1000


def test_the_provision_lib_sources_the_dock_lib():
    text = PROVISION.read_text()
    assert re.search(r'(?m)^  \. "\$\(dirname "\$\{BASH_SOURCE\[0\]\}"\)/obs-box-avsync-dock\.sh"$', text)


def test_the_bundle_install_never_prunes_the_dock():
    r = _bash("strih_lx_obs_plugin_prune_list", box="stream-snv")
    for pat in r.stdout.split():
        assert not re.fullmatch(pat.replace(".", r"\.").replace("*", ".*"), "obs-audio-video-sync-dock.so"), pat
