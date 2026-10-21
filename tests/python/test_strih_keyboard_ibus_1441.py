"""issue 1441 (owner 21.10.2026, stream-pp): the panel keyboard item could not switch to Slovak.

im-config starts ibus-daemon in the kiosk session. With org.freedesktop.ibus.general
use-system-keyboard-layout false (the default), ibus overwrote the X keymap with its engine's `us` at
every login, so :0 carried ONE group and the panel's click locked US again (the stream-pp journal:
`layout US -> US`). Read live on stream-pp, strih-pp and strih-snv the same day. setup-strih step 16g
now sets it true BEFORE its live setxkbmap, and verify item 43's keyboard row grades it
(scripts/lib/strih-keyboard.sh, design comment 6320020790).

The operator surface fixture (fake X, fake gsettings, fake dbus-run-session) is the issue-1418 one.
"""

import os
import socket
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from test_strih_operator_1418 import KEYBOARD_OK, ME, REPO, _bash, op  # noqa: E402,F401

SCHEMA = "org.freedesktop.ibus.general"
KEY = "use-system-keyboard-layout"


def _ibus(o, value):
    """ibus installed (its schema listed) with KEY = VALUE."""
    (o.st / "ibus-schemas").write_text("org.gnome.desktop.interface\n%s\norg.gtk.Settings.Debug\n" % SCHEMA)
    (o.st / "ibus-value").write_text(value + "\n")


def _log(o):
    p = o.st / "kbd-log"
    return p.read_text().splitlines() if p.exists() else []


def _install(o, tmp_path, user="operator1", extra=None):
    kb = tmp_path / "kb"
    kb.write_text(KEYBOARD_OK)
    env = {**o.env(), "STRIH_OPERATOR_KEYBOARD_FILE": str(kb), **(extra or {})}
    return _bash('strih_operator_keyboard_install "$1" "$2"', env=env, args=(user, str(o.home)))


# --- the grade: verify item 43, row (keyboard) ---------------------------------------------------------

def test_ibus_that_resets_the_layout_fails_the_keyboard_row(op):
    """The file and the live layout read fine right after a setxkbmap; ibus puts :0 back on `us` at the
    next login, so the row FAILs on the setting itself and names it."""
    _ibus(op, "false")
    st, d = op.row("keyboard")
    assert st == "FAIL" and "ibus" in d and KEY in d and "16g" in d, d


def test_ibus_named_as_the_cause_when_the_live_layout_already_lost_slovak(op):
    """The 21.10.2026 state of all three boxes: :0 on `us`, ibus false. The row names the cause."""
    _ibus(op, "false")
    (op.st / "xkb").write_text("rules:      evdev\nmodel:      pc105\nlayout:     us\noptions:    grp:alt_shift_toggle\n")
    st, d = op.row("keyboard")
    assert st == "FAIL" and KEY in d, d


def test_ibus_that_keeps_the_system_layout_passes(op):
    _ibus(op, "true")
    st, d = op.row("keyboard")
    assert st == "OK" and "ibus" in d, d


def test_a_box_without_ibus_is_graded_on_the_file_and_the_live_layout(op):
    st, d = op.row("keyboard")  # the fixture lists no ibus schema: ibus is not installed
    assert st == "OK", d
    assert not any(ln.startswith("gsettings get") for ln in _log(op)), _log(op)


def test_a_box_without_gsettings_is_graded_on_the_file_and_the_live_layout(op):
    env = {**op.env(), "STRIH_OPERATOR_GSETTINGS": str(op.tmp / "no-such-gsettings")}
    r = _bash('strih_operator_grade_rows "$1" "$2" "$3"', env=env, args=(str(REPO), str(op.home), "operator1"))
    rows = [ln for ln in r.stdout.splitlines() if "(keyboard)" in ln]
    assert len(rows) == 1 and rows[0].startswith("OK|"), r.stdout + r.stderr


def test_an_unreadable_ibus_setting_fails(op):
    _ibus(op, "true")
    (op.st / "gsettings-fails").write_text("")
    st, d = op.row("keyboard")
    assert st == "FAIL" and "ibus" in d and "not read" in d, d


def test_an_ibus_setting_that_is_not_a_boolean_fails(op):
    _ibus(op, "'maybe'")
    st, d = op.row("keyboard")
    assert st == "FAIL" and "ibus" in d, d


# --- the setup: step 16g -------------------------------------------------------------------------------

def test_install_tells_ibus_to_keep_the_layout_before_the_live_setxkbmap(op, tmp_path):
    _ibus(op, "false")
    r = _install(op, tmp_path)
    assert r.returncode == 0, r.stdout + r.stderr
    log = _log(op)
    want = "gsettings set %s %s true" % (SCHEMA, KEY)
    assert want in log, log
    xkb = [i for i, ln in enumerate(log) if ln.startswith("setxkbmap -layout")]
    assert xkb and log.index(want) < xkb[0], log
    assert (op.st / "ibus-value").read_text().strip() == "true"
    assert "ibus" in r.stdout, r.stdout
    # The grade reads the result back as OK.
    assert op.row("keyboard")[0] == "OK"


def test_install_leaves_ibus_alone_when_it_already_keeps_the_layout(op, tmp_path):
    _ibus(op, "true")
    r = _install(op, tmp_path)
    assert r.returncode == 0, r.stdout + r.stderr
    assert not any(ln.startswith("gsettings set") for ln in _log(op)), _log(op)


def test_install_without_ibus_sets_nothing_and_still_applies_the_layout(op, tmp_path):
    r = _install(op, tmp_path)
    assert r.returncode == 0, r.stdout + r.stderr
    assert not any(ln.startswith("gsettings set") for ln in _log(op)), _log(op)
    assert any(ln.startswith("setxkbmap -layout") for ln in _log(op)), _log(op)


def test_a_failed_ibus_set_warns_and_setup_goes_on(op, tmp_path):
    _ibus(op, "false")
    (op.st / "gsettings-set-fails").write_text("")
    r = _install(op, tmp_path)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "WARN" in r.stdout and KEY in r.stdout, r.stdout
    assert any(ln.startswith("setxkbmap -layout") for ln in _log(op)), _log(op)


def test_gsettings_runs_on_the_operators_session_bus_when_one_runs(op, tmp_path):
    """A running session: the set goes over its bus, so the live ibus takes it at once."""
    _ibus(op, "false")
    uid = str(os.getuid())
    run = tmp_path / "run"
    (run / uid).mkdir(parents=True)
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.bind(str(run / uid / "bus"))
    try:
        r = _install(op, tmp_path, user=ME, extra={"STRIH_OPERATOR_RUN_USER_DIR": str(run)})
    finally:
        sock.close()
    assert r.returncode == 0, r.stdout + r.stderr
    buses = (op.st / "gsettings-bus").read_text().split()
    assert buses and set(buses) == {"unix:path=%s" % (run / uid / "bus")}, buses


def test_gsettings_uses_a_private_bus_without_a_session(op, tmp_path):
    _ibus(op, "false")
    r = _install(op, tmp_path, extra={"STRIH_OPERATOR_RUN_USER_DIR": str(tmp_path / "no-run")})
    assert r.returncode == 0, r.stdout + r.stderr
    buses = (op.st / "gsettings-bus").read_text().split()
    assert buses and set(buses) == {"private"}, buses
