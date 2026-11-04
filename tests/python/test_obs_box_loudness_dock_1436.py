"""issue 1436 -- the LUFS meter (norihiro obs-loudness-dock) on every stream-role Linux OBS box.

Owner 4.11.2026: the Windows stream OBS had a LUFS meter for the operator; the Linux migration lost it.
The pinned obs release (v0.4.13) carries `loudness-dock.so` + its locale, so setup-strih step 4's bundle
install puts it on every Linux OBS box; OBS opens a plugin dock hidden + floating until the saved
[BasicWindow] DockState places it. These tests pin:
  * scripts/obs_dock_state.py's `--marker KEY` (each seeded dock its own first-seed marker in the same
    [CameraBox] section; the default stays `AvsyncDockSeeded`);
  * scripts/lib/obs-box-loudness-dock.sh over the shared scripts/lib/obs-box-plugin-dock.sh: the step-16g
    seed (stream role only, never while OBS runs, after the A/V-sync dock so the meter joins it in the
    right area) and verify item 47's three loudness rows;
  * the setup / verify wiring and their 1000-line budgets.
"""

import base64
import importlib.util
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
LIB = SCRIPTS / "lib" / "obs-box-loudness-dock.sh"
SHARED = SCRIPTS / "lib" / "obs-box-plugin-dock.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
SEED = SCRIPTS / "obs_dock_state.py"
RULE = REPO / ".claude" / "rules" / "av-sync-dock-linux.md"
FIX = Path(__file__).resolve().parent / "fixtures" / "obs_dock_state_1470"
AVSYNC = "net.nagater.obs-audio-video-sync-dock..main"
LOUD = "net.nagater.obs-loudness-dock..main"
ME = os.environ.get("USER") or "nobody"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_LX_DANTESYNC_ROLE", "STRIH_BOXES_DIR", "OBS_FLEET")


def _mod():
    spec = importlib.util.spec_from_file_location("ods_1436", SEED)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def _bash(body, env=None, box="stream-pp", args=()):
    script = 'set -euo pipefail\n. "$1"\nstrih_box_load "$2"\n. "$3"\nshift 3\n' + body
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    r = subprocess.run(["bash", "-c", script, "harness", str(FACTS), box, str(PROVISION), *args],
                       capture_output=True, text=True, env=e, timeout=120)
    assert "unbound variable" not in r.stderr, r.stderr
    return r


def _obs(tmp_path, running):
    p = tmp_path / ("obs-up" if running else "obs-down")
    p.write_text("#!/bin/bash\nexit %d\n" % (0 if running else 1))
    p.chmod(0o755)
    return {"STRIH_OPERATOR_OBS_RUNNING_CMD": str(p)}


def _ini(tmp_path, box="stream-pp"):
    p = tmp_path / "user.ini"
    shutil.copy(FIX / f"{box}.user.ini", p)
    return p


def _cli(*args):
    r = subprocess.run(["python3", str(SEED), *args], capture_output=True, text=True)
    return r.returncode, r.stdout.strip()


def _items(ini):
    line = [l for l in ini.read_text().splitlines() if l.startswith("DockState=")][0]
    st = _mod().parse(base64.b64decode(line.split("=", 1)[1]))
    right = [a for a in st["areas"] if a["pos"] == 1]
    return st, right


def _section(ini):
    lines = ini.read_text().splitlines()
    return lines[lines.index("[CameraBox]") + 1:] if "[CameraBox]" in lines else []


# --- obs_dock_state.py --marker ---------------------------------------------------------------------

def test_the_marker_option_writes_its_own_key(tmp_path):
    ini = _ini(tmp_path)
    assert _cli("seed", "--ini", str(ini), "--dock", LOUD, "--marker", "LoudnessDockSeeded") == (0, "seeded")
    assert "LoudnessDockSeeded=true" in _section(ini) and "AvsyncDockSeeded=true" not in _section(ini)
    assert _cli("check", "--ini", str(ini), "--dock", LOUD, "--marker", "LoudnessDockSeeded") == (0, "visible")


def test_the_default_marker_stays_the_avsync_one(tmp_path):
    ini = _ini(tmp_path)
    assert _cli("seed", "--ini", str(ini), "--dock", AVSYNC) == (0, "seeded")
    assert _section(ini) == ["AvsyncDockSeeded=true"]


def test_two_markers_share_one_section_never_a_second_header(tmp_path):
    ini = _ini(tmp_path)
    _cli("seed", "--ini", str(ini), "--dock", AVSYNC)
    _cli("seed", "--ini", str(ini), "--dock", LOUD, "--marker", "LoudnessDockSeeded")
    assert ini.read_text().count("[CameraBox]") == 1
    assert sorted(_section(ini)) == ["AvsyncDockSeeded=true", "LoudnessDockSeeded=true"]


def _floated(ini, name):
    """ini's DockState with `name` held in OBS's hidden + floating add-time default in the right area."""
    m = _mod()
    text = ini.read_text()
    line = [l for l in text.splitlines() if l.startswith("DockState=")][0]
    blob = bytearray(m.seed(base64.b64decode(line.split("=", 1)[1]), name))
    item = [i for i in m.parse(bytes(blob))["items"] if i["name"] == name][0]
    blob[item["flags_offset"]] = 0x02
    ini.write_text(text.replace(line, "DockState=" + base64.b64encode(bytes(blob)).decode()))


def test_the_avsync_marker_never_blocks_the_loudness_redock(tmp_path):
    # A box already seeded for the A/V-sync dock (issue 1470) whose OBS ran once with the loudness
    # plugin: the meter sits in OBS's hidden default and is still re-docked -- its own marker decides.
    ini = _ini(tmp_path)
    _floated(ini, LOUD)
    ini.write_text(ini.read_text() + "\n[CameraBox]\nAvsyncDockSeeded=true\n")
    assert _cli("check", "--ini", str(ini), "--dock", LOUD, "--marker", "LoudnessDockSeeded") == (0, "default-hidden")
    assert _cli("seed", "--ini", str(ini), "--dock", LOUD, "--marker", "LoudnessDockSeeded") == (0, "redocked")
    assert sorted(_section(ini)) == ["AvsyncDockSeeded=true", "LoudnessDockSeeded=true"]
    assert _cli("check", "--ini", str(ini), "--dock", LOUD, "--marker", "LoudnessDockSeeded") == (0, "visible")


def test_after_its_own_seed_a_closed_meter_is_kept(tmp_path):
    ini = _ini(tmp_path)
    _floated(ini, LOUD)
    ini.write_text(ini.read_text() + "\n[CameraBox]\nLoudnessDockSeeded=true\n")
    snap = ini.read_bytes()
    assert _cli("check", "--ini", str(ini), "--dock", LOUD, "--marker", "LoudnessDockSeeded") == (0, "hidden")
    assert _cli("seed", "--ini", str(ini), "--dock", LOUD, "--marker", "LoudnessDockSeeded") == (0, "present")
    assert ini.read_bytes() == snap


@pytest.mark.parametrize("bad", ["", "Has Space", "a=b", "[x]", "x\ny"])
def test_a_marker_that_is_not_a_plain_key_is_refused(tmp_path, bad):
    ini = _ini(tmp_path)
    snap = ini.read_bytes()
    r = subprocess.run(["python3", str(SEED), "seed", "--ini", str(ini), "--dock", LOUD, "--marker", bad],
                       capture_output=True, text=True)
    assert r.returncode != 0 and ini.read_bytes() == snap


# --- the lib: setup step 16g -----------------------------------------------------------------------

def test_the_meter_joins_the_avsync_dock_in_the_right_area_on_stream_pp(tmp_path):
    ini = _ini(tmp_path)
    r = _bash('obs_box_avsync_dock_seed "$1" "$2"; obs_box_loudness_dock_seed "$1" "$2"',
              env=_obs(tmp_path, False), args=(str(ini), ME))
    assert r.returncode == 0, r.stderr
    assert "LUFS meter (loudness dock) seeded visible + docked right" in r.stdout, r.stdout
    st, right = _items(ini)
    assert len(right) == 1
    names = [i["name"] for i in st["items"]]
    # the meter lands right after the A/V-sync dock, both visible + docked
    assert names.index(LOUD) == names.index(AVSYNC) + 1
    for n in (AVSYNC, LOUD):
        assert [i["flags"] for i in st["items"] if i["name"] == n] == [1]
    assert sorted(_section(ini)) == ["AvsyncDockSeeded=true", "LoudnessDockSeeded=true"]


def test_stream_snv_keeps_its_ported_meter(tmp_path):
    ini = _ini(tmp_path, "stream-snv")
    before = [l for l in ini.read_text().splitlines() if l.startswith("DockState=")]
    r = _bash('obs_box_loudness_dock_seed "$1" "$2"', env=_obs(tmp_path, False), args=(str(ini), ME), box="stream-snv")
    assert r.returncode == 0 and "LUFS meter (loudness dock): the saved OBS layout already places it" in r.stdout, r.stdout
    assert [l for l in ini.read_text().splitlines() if l.startswith("DockState=")] == before
    assert _section(ini) == ["LoudnessDockSeeded=true"]


def test_the_meter_seed_never_writes_while_obs_runs(tmp_path):
    ini = _ini(tmp_path)
    snap = ini.read_bytes()
    r = _bash('obs_box_loudness_dock_seed "$1" "$2"', env=_obs(tmp_path, True), args=(str(ini), ME))
    assert r.returncode == 0 and "WARN: OBS is running" in r.stdout and "LUFS meter (loudness dock) was NOT seeded" in r.stdout
    assert ini.read_bytes() == snap


@pytest.mark.parametrize("box", ["strih-snv", "strih-pp"])
def test_the_strih_role_skips_the_meter_seed(tmp_path, box):
    ini = _ini(tmp_path)
    snap = ini.read_bytes()
    r = _bash('obs_box_loudness_dock_seed "$1" "$2"', env=_obs(tmp_path, False), args=(str(ini), ME), box=box)
    assert r.returncode == 0 and "LUFS meter (loudness dock) layout: SKIP -- " in r.stdout, r.stdout
    assert "stream operator" in r.stdout
    assert ini.read_bytes() == snap


def test_an_unreadable_user_ini_is_a_warn_and_untouched(tmp_path):
    ini = tmp_path / "user.ini"
    ini.write_text("[BasicWindow]\nDockState=AAAA\n")
    r = _bash('obs_box_loudness_dock_seed "$1" "$2"', env=_obs(tmp_path, False), args=(str(ini), ME))
    assert r.returncode == 0 and "WARN: the LUFS meter (loudness dock) could not be seeded" in r.stdout
    assert ini.read_text() == "[BasicWindow]\nDockState=AAAA\n"


@pytest.mark.parametrize("box,runs", [("stream-pp", True), ("stream-snv", True), ("strih-snv", False), ("strih-pp", False)])
def test_the_step_gate_is_the_stream_role(box, runs):
    r = _bash('if WHY="$(obs_box_step_applies loudness-dock)"; then echo RUN; else echo "SKIP $WHY"; fi', box=box)
    assert r.stdout.startswith("RUN" if runs else "SKIP"), r.stdout


# --- the lib: verify item 47 -----------------------------------------------------------------------

LOG_WITH = """02:35:33.121:   Loaded Modules:
02:35:33.121:     obs-vban.so
02:35:33.121:     obs-audio-video-sync-dock.so
02:35:33.121:     loudness-dock.so
02:35:33.121:     distroav.so
02:35:33.121: ---------------------------------
"""
LOG_WITHOUT = LOG_WITH.replace("02:35:33.121:     loudness-dock.so\n", "")


def _prefix(tmp_path, so=True, locale=True):
    lib = tmp_path / "lib"
    share = tmp_path / "share"
    (lib / "obs-plugins").mkdir(parents=True)
    loc = share / "obs" / "obs-plugins" / "loudness-dock" / "locale"
    loc.mkdir(parents=True)
    if so:
        (lib / "obs-plugins" / "loudness-dock.so").write_bytes(b"\x7fELF")
    if locale:
        (loc / "en-US.ini").write_text("LoudnessDock.Title=\"Loudness\"\n")
    return lib, share


def _rows(tmp_path, log_text, ini, lib, share, box="stream-pp"):
    log = tmp_path / "obs.txt"
    if log_text is None:
        log_arg = ""
    else:
        log.write_text(log_text)
        log_arg = str(log)
    r = _bash('obs_box_loudness_dock_grade_rows "$1" "$2" "$3" "$4"', args=(str(lib), str(share), log_arg, str(ini)),
              box=box)
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


def test_a_healthy_stream_box_reads_three_ok_rows(tmp_path):
    lib, share = _prefix(tmp_path)
    rows = _rows(tmp_path, LOG_WITH, _ini(tmp_path, "stream-snv"), lib, share, box="stream-snv")
    assert [r.split("|")[0] for r in rows] == ["OK", "OK", "OK"], rows
    assert rows[0].startswith("OK|(loudness-dock) the LUFS meter (loudness dock) plugin is installed: ")
    assert rows[1].startswith("OK|(loudness-dock-loaded) ") and "loudness-dock.so" in rows[1]
    assert rows[2].startswith("OK|(loudness-dock-layout) ")


@pytest.mark.parametrize("so,locale", [(False, True), (True, False), (False, False)])
def test_a_missing_plugin_or_locale_fails_the_installed_row(tmp_path, so, locale):
    lib, share = _prefix(tmp_path, so, locale)
    rows = _rows(tmp_path, LOG_WITH, _ini(tmp_path, "stream-snv"), lib, share)
    assert rows[0].startswith("FAIL|(loudness-dock) the LUFS meter (loudness dock) plugin is not installed"), rows
    # the plugin comes from the pinned obs release bundle (setup-strih step 4), never its own install
    assert "obs release" in rows[0] and "setup-strih.sh step 4" in rows[0]


def test_a_log_without_the_module_fails_the_loaded_row(tmp_path):
    lib, share = _prefix(tmp_path)
    rows = _rows(tmp_path, LOG_WITHOUT, _ini(tmp_path, "stream-snv"), lib, share)
    assert rows[1].startswith("FAIL|(loudness-dock-loaded) ") and "restart strih-obs.service" in rows[1]


def test_an_avsync_only_log_never_reads_as_the_meter_loaded(tmp_path):
    # `obs-audio-video-sync-dock.so` is not `loudness-dock.so`: the module match is the whole name
    r = _bash('printf "x:   Loaded Modules:\\nx:     xloudness-dock.so\\nx: -----\\n" | obs_box_loudness_dock_loaded_verdict')
    assert r.stdout.strip() == "not-loaded", r.stdout


def test_the_layout_row_reads_each_state(tmp_path):
    lib, share = _prefix(tmp_path)
    row = _rows(tmp_path, LOG_WITH, _ini(tmp_path, "stream-pp"), lib, share)[2]
    assert row.startswith("FAIL|(loudness-dock-layout) ") and "has no LUFS meter (loudness dock)" in row, row
    assert "re-run setup-strih.sh step 16g (issue 1436)" in row
    ini = _ini(tmp_path)
    _floated(ini, LOUD)
    ini.write_text(ini.read_text() + "\n[CameraBox]\nLoudnessDockSeeded=true\n")
    row = _rows(tmp_path, LOG_WITH, ini, lib, share)[2]
    assert row.startswith("NOTE|(loudness-dock-layout) the LUFS meter (loudness dock) is hidden in "), row


def test_the_strih_role_grades_the_plugin_but_not_the_layout(tmp_path):
    lib, share = _prefix(tmp_path)
    rows = _rows(tmp_path, LOG_WITH, _ini(tmp_path), lib, share, box="strih-snv")
    assert rows[0].startswith("OK|(loudness-dock) ") and rows[1].startswith("OK|(loudness-dock-loaded) ")
    assert rows[2].startswith("NOTE|(loudness-dock-layout) not graded: "), rows


def test_the_report_maps_rows_to_the_callers_verdicts_and_counts_them(tmp_path):
    lib, share = _prefix(tmp_path, so=False)
    body = ('ok() { echo "PASS $1"; }; bad() { echo "FAIL $1"; }; note() { echo "NOTE $1"; }\n'
            'rc=0; obs_box_loudness_dock_grade_report "$1" "$2" "" "$3" || rc=$?; echo "rc=$rc"\n')
    r = _bash(body, args=(str(lib), str(share), str(_ini(tmp_path, "stream-snv"))), box="stream-snv")
    out = r.stdout.splitlines()
    assert out[0].startswith("FAIL (loudness-dock) ") and out[1].startswith("NOTE (loudness-dock-loaded) ")
    assert out[2].startswith("PASS (loudness-dock-layout) ") and out[3] == "rc=0", out


# --- the wiring --------------------------------------------------------------------------------------

def test_setup_seeds_the_meter_after_the_avsync_dock_in_step_16g():
    s = SETUP.read_text()
    body = s[s.index('step "16g" '):s.index('\nstep "16h" ')]
    av = 'obs_box_avsync_dock_seed "${OBS_CFG}/user.ini" "$DESKTOP_USER"'
    call = 'obs_box_loudness_dock_seed "${OBS_CFG}/user.ini" "$DESKTOP_USER"'
    assert s.count(call) == 1 and call in body
    assert body.index(av) < body.index(call)
    assert len(s.splitlines()) <= 1000


def test_verify_grades_the_meter_on_the_item_47_line():
    v = VERIFY.read_text()
    j = v.index("# 47) issue 1470")
    line = v[v.rindex("\n", 0, j) + 1:v.index("\n", j)]
    call = ('obs_box_loudness_dock_grade_report "${STRIH_LIBDIR:-/usr/lib/x86_64-linux-gnu}" "${STRIH_SHAREDIR:-/usr/share}" '
            '"${LOG:-}" "${USER_HOME}/.config/obs-studio/user.ini" || bad "(loudness-dock) the grader did not print all of its rows"')
    assert call in line
    assert line.index("obs_box_avsync_dock_grade_report ") < line.index(call)
    assert "issue 1436" in line[line.index("#"):]
    assert len(v.splitlines()) <= 1000


def test_the_provision_lib_sources_the_meter_lib_and_one_shared_engine():
    text = PROVISION.read_text()
    assert re.search(r'(?m)^  \. "\$\(dirname "\$\{BASH_SOURCE\[0\]\}"\)/obs-box-loudness-dock\.sh"$', text)
    # one engine: both dock libs are specs over the shared plugin-dock lib, never a second copy
    for f in (LIB, SCRIPTS / "lib" / "obs-box-avsync-dock.sh"):
        t = f.read_text()
        assert "obs-box-plugin-dock.sh" in t, f
        assert "python3 " not in t and "awk " not in t, f
    assert "obs_box_plugin_dock_seed()" in SHARED.read_text()


def test_the_bundle_install_never_prunes_the_meter():
    r = _bash("strih_lx_obs_plugin_prune_list", box="stream-snv")
    for pat in r.stdout.split():
        assert not re.fullmatch(pat.replace(".", r"\.").replace("*", ".*"), "loudness-dock.so"), pat


def test_the_rule_names_the_meter():
    t = RULE.read_text()
    assert LOUD in t and "LoudnessDockSeeded" in t and "issue 1436" in t
    assert "scripts/lib/obs-box-loudness-dock.sh" in t and "scripts/lib/obs-box-plugin-dock.sh" in t
