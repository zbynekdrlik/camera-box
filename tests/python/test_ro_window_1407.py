"""issue 1407 -- ONE verified close for every read-only-root rw window (scripts/lib/ro-window.sh).

Issue 1405 fixed one rw window (the cam2-painter handoff): only the enable ran inside it, the root
mode was READ after the ro remount, and a root left writable failed loud naming the writers. The
same unverified, swallowed close sat in deploy-fleet, the dantesync upgrade/rollback, the bkshading
relay mode + deploy and the ndi-discovery apply. Main's design (issue comment 5993388328): one
emitter lib, every site calls it, and every site starts/restarts nothing before the verified close.

This file pins the emitter itself by RUNNING its text on a fake box (tests/python/ro_window_fakes_1407.py),
plus two repo-wide guards: the static window pin per text-emitting site (nothing started between the
rw open and the close) and the sweep (no `remount,ro` swallow anywhere under scripts/ outside the
lib). Tier-0 (#557): no cargo, no rig.
"""
import functools
import os
import re
import subprocess
import sys

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ro_window_fakes_1407 import LIB, ROOT, build, index, log, make_box, root, run_text  # noqa: E402

_RO_WINDOW = LIB / "ro-window.sh"
_MODES = ["set -e", "set -euo pipefail"]


def _close_text(tag="issue 1407", box="cam9", consequence="so nothing is started.", hint="fix it by hand."):
    return build(f'. "{_RO_WINDOW}"\nro_window_close_cmds "$1" "$2" "$3" "$4"', tag, box, consequence, hint)


@pytest.fixture
def box(tmp_path):
    return make_box(tmp_path, root="rw")  # the window is OPEN when the close runs


# ---- the close itself --------------------------------------------------------------------------- #


@pytest.mark.parametrize("strict", _MODES)
def test_a_clean_close_leaves_the_root_ro_and_runs_on(box, strict):
    proc = run_text(box, _close_text() + "\necho AFTER-THE-CLOSE opts=$_row_opts", strict)
    calls = log(box)
    assert proc.returncode == 0, f"stdout={proc.stdout!r}\nstderr={proc.stderr!r}\nlog={calls}"
    assert root(box) == "ro", calls
    sync = index(calls, "sync -f /")
    ro = index(calls, "mount -o remount,ro /")
    read = index(calls, "findmnt -no OPTIONS /")
    assert sync < ro < read, f"sync, then the ro remount, then the mode READ:\n{calls}"
    assert "AFTER-THE-CLOSE opts=ro,relatime" in proc.stdout, proc.stdout
    assert "FAIL" not in proc.stderr, proc.stderr


@pytest.mark.parametrize("strict", _MODES)
def test_a_busy_close_fails_loud_naming_the_writer_and_stops_the_text(box, strict):
    proc = run_text(box, _close_text() + "\necho NEVER-REACHED", strict, FAKE_RO_FAIL="1")
    assert proc.returncode == 1, proc.stderr
    assert "NEVER-REACHED" not in proc.stdout, "a failed close must stop the text: nothing after it runs"
    assert "FAIL: [issue 1407] cam9's root is NOT read-only" in proc.stderr, proc.stderr
    assert "mount point is busy" in proc.stderr, f"the mount error is named:\n{proc.stderr}"
    assert "so nothing is started." in proc.stderr, "the caller's consequence is printed"
    assert "76355" in proc.stderr and "systemd-journal" in proc.stderr, (
        f"the writer past PID 1 and the kernel threads is named:\n{proc.stderr}"
    )
    assert "kworker/3:0-events" not in proc.stderr, "only the WRITERS are listed, never every process on /"
    assert "/usr/local/bin/bkshading-relay (deleted)" in proc.stderr, "the deleted-but-open holder is named"
    assert proc.stderr.rstrip().splitlines()[-1] == "FAIL: [issue 1407] fix it by hand.", proc.stderr


@pytest.mark.parametrize("strict", _MODES)
def test_the_close_trusts_the_mode_read_never_the_mount_exit_code(box, strict):
    proc = run_text(box, _close_text() + "\necho NEVER-REACHED", strict, FAKE_RO_LIES="1")
    assert proc.returncode == 1, "a zero-exit ro remount that left the root rw must fail loud"
    assert "NEVER-REACHED" not in proc.stdout
    assert "-> rw" in proc.stderr, proc.stderr


@pytest.mark.parametrize("strict", _MODES)
def test_an_unreadable_root_mode_is_a_failure_never_ro(box, strict):
    # findmnt fails and the /proc/mounts fallback fails too (a failing awk, so the host's own
    # /proc/mounts is never read): `unknown`, never assumed read-only.
    (box["stub"] / "awk").unlink()
    (box["stub"] / "awk").write_text(f"#!{sys.executable}\nimport sys\nsys.exit(2)\n")
    (box["stub"] / "awk").chmod(0o755)
    proc = run_text(box, _close_text() + "\necho NEVER-REACHED", strict, FAKE_FINDMNT_EMPTY="1")
    assert proc.returncode == 1, proc.stderr
    assert "NEVER-REACHED" not in proc.stdout
    assert "-> unknown" in proc.stderr, proc.stderr


def test_a_missing_fuser_is_named_never_read_as_no_writer(box):
    (box["stub"] / "fuser").unlink()
    proc = run_text(box, _close_text(), FAKE_RO_FAIL="1")
    assert proc.returncode == 1
    assert "fuser printed no listing" in proc.stderr, proc.stderr
    assert "(none: no process holds a file open for writing on /)" not in proc.stderr, proc.stderr


def test_a_failing_sync_is_reported_but_the_mode_read_decides(box):
    (box["stub"] / "sync").unlink()  # no sync on the box at all: the read still decides
    proc = run_text(box, _close_text() + "\necho AFTER")
    assert proc.returncode == 0, proc.stderr
    assert "AFTER" in proc.stdout and root(box) == "ro"
    other = box["state"].parent / "b2"
    other.mkdir()
    busy = make_box(other, root="rw")
    (busy["stub"] / "sync").unlink()
    proc = run_text(busy, _close_text(), FAKE_RO_FAIL="1")
    assert "sync rc=127" in proc.stderr, "a failed sync is reported on the FAIL line:\n" + proc.stderr


def test_the_box_name_consequence_and_hint_may_read_state_on_the_box(box):
    text = _close_text(box="cam2", consequence="state now: '$(echo from-the-box)'.", hint="then re-run X.")
    proc = run_text(box, text, FAKE_RO_FAIL="1")
    assert "cam2's root is NOT read-only" in proc.stderr
    assert "state now: 'from-the-box'." in proc.stderr, "a $ in the consequence expands ON THE BOX"


@pytest.mark.parametrize("strict", _MODES)
def test_embedding_never_glues_the_following_command(box, tmp_path, strict):
    marker = tmp_path / "next-command-ran"
    marker.write_text("x")
    # `$(...)` strips the trailing newline, so the next command lands on the SAME line as the
    # emitted text's last statement. It must still run as its own statement.
    text = build(f'. "{_RO_WINDOW}"\nprintf \'%s\\n\' "$(ro_window_close_cmds t b c h) rm -f {marker}"')
    proc = run_text(box, text, strict)
    assert proc.returncode == 0, proc.stderr
    assert not marker.exists(), "the command after the embedded close never ran"


def test_the_close_names_remount_ro_once_and_never_systemctl():
    # A caller's own anchors count `remount,ro /` and `systemctl` lines (bkshading-relay-mode.sh:
    # every systemctl line ends `|| true`; the bkshading deploy tests count close calls).
    text = _close_text(consequence="c", hint="h")
    assert text.count("remount,ro /") == 1, text
    assert "systemctl" not in text, text
    assert "is-active" not in text, text


def test_the_close_reads_the_mode_through_the_shared_ro_root_canon():
    text = _close_text()
    assert text.startswith("ro_root_mount_mode () \n{"), "the canon's definition is emitted first"
    assert '"$(ro_root_mount_mode "$_row_opts")"' in text, text
    lib = _RO_WINDOW.read_text()
    assert "ro-root.sh" in lib, "the mode reading is sourced from the one canon, never re-typed"
    assert "ro | ro,*" not in lib, "no second copy of the first-token reading"


def test_ro_root_canon_stays_free_of_grep_awk_sed():
    # The issue-1311 heredoc test runs setup-device STEP 18 with `grep` stubbed out.
    body = "\n".join(ln for ln in (LIB / "ro-root.sh").read_text().splitlines() if not ln.lstrip().startswith("#"))
    for tool in ("grep", "awk", "sed"):
        assert not re.search(rf"\b{tool}\b", body), tool


# ---- the dev1-side holder summary ---------------------------------------------------------------- #


def test_holders_summary_names_the_writers_and_the_deleted_holders(box):
    proc = run_text(box, _close_text(), FAKE_RO_FAIL="1")
    out = build(f'. "{_RO_WINDOW}"\nro_window_holders "$1"', proc.stderr).strip()
    assert out == "systemd-journal[76355]; relay[4242] /usr/local/bin/bkshading-relay", out


def test_holders_summary_of_nothing_is_empty():
    assert build(f'. "{_RO_WINDOW}"\nro_window_holders ""') == ""
    assert build(f'. "{_RO_WINDOW}"\nro_window_holders "ssh: connect to host x port 22: refused"') == ""


def test_deleted_holders_summary_is_pure():
    text = ("COMMAND PID USER FD TYPE DEVICE SIZE/OFF NLINK NODE NAME\n"
            "bkshading 4242 root txt REG 8,2 9000 0 1234 /usr/local/bin/bkshading-relay (deleted)\n"
            "journald 99 root 5w REG 8,2 1 0 55 /var/x (deleted)\n")
    out = build(f'. "{_RO_WINDOW}"\nro_window_deleted_holders "$1"', text).strip()
    assert out == "bkshading[4242] /usr/local/bin/bkshading-relay; journald[99] /var/x", out


# ---- every text-emitting site: nothing started inside its window --------------------------------- #

_STARTS = re.compile(r"systemctl[^\n;|&#]*\b(start|restart)\b|enable\s+--now|systemd-run")
_INLINE_CLOSE = '_row_ro_err="$(mount -o remount,ro / 2>&1)"'


@functools.lru_cache(maxsize=1)
def _site_texts():
    """site -> (emitted text, the marker where its window CLOSES): the shared close inline, or the
    standalone call of the site's close function (the dantesync programs and the ndi apply wrap the
    shared close in a function their EXIT trap also calls)."""
    persist = LIB / "cam2-painter-ro-persist.sh"
    relay = LIB / "bkshading-relay-mode.sh"
    ndi = LIB / "ndi-discovery.sh"
    upgrade = ROOT / "scripts" / "dantesync-fleet-upgrade.sh"
    return {
        "persist enable-now": (build(f'. "{persist}"\ncam2_painter_persist_state_cmds enable-now'), _INLINE_CLOSE),
        "persist disable": (build(f'. "{persist}"\ncam2_painter_persist_state_cmds disable'), _INLINE_CLOSE),
        "relay-mode stop": (build(f'. "{relay}"\nbkshading_relay_mode_stop_cmds'), _INLINE_CLOSE),
        "relay-mode start": (build(f'. "{relay}"\nbkshading_relay_mode_start_cmds'), _INLINE_CLOSE),
        "ndi apply": (build(f'. "{ndi}"\nndi_discovery_cambox_apply_remote_snippet'), "\n_ndi_restore_ro\n"),
        "dantesync upgrade": (build(f'set +e\n. "{upgrade}"\nset -e\ndantesync_linux_upgrade_cmd 1.15.0'),
                              "\n_dantesync_remount_ro\n"),
        "dantesync rollback": (build(f'set +e\n. "{upgrade}"\nset -e\ndantesync_linux_rollback_cmd'),
                               "\n_dantesync_remount_ro\n"),
    }


_SITES = ["dantesync rollback", "dantesync upgrade", "ndi apply", "persist disable", "persist enable-now",
          "relay-mode start", "relay-mode stop"]


def test_the_site_list_is_complete():
    assert sorted(_site_texts()) == _SITES


@pytest.mark.parametrize("site", _SITES)
def test_every_text_site_closes_with_the_shared_emitter_and_starts_nothing_inside(site):
    text, end = _site_texts()[site]
    assert _INLINE_CLOSE in text, f"{site}: the shared close is not emitted:\n{text}"
    rw = text.find("mount -o remount,rw /")
    assert rw >= 0, f"{site}: no rw window:\n{text}"
    close = text.find(end, rw)
    assert close > rw, f"{site}: the window is never closed (no {end!r} after the rw open):\n{text}"
    # Only the window body counts: a function DEFINED inside it (a self-heal, the close helper)
    # runs later, so strip the bodies of `name() { ... }` definitions before looking for a start.
    body = re.sub(r"\n(\w+)\(\) \{\n.*?\n\}\n", "\n", text[rw:close], flags=re.S)
    hits = [m.group(0) for m in _STARTS.finditer(body)]
    assert hits == [], f"{site}: a start/restart/enable --now sits INSIDE the rw window: {hits}\n{body}"
    after = text[close:]
    assert not re.search(r"remount,ro\b[^\n]*(2>\s*/dev/null|\|\|\s*true)", after), (
        f"{site}: a swallowed ro close after the shared one:\n{after}")


# ---- the sweep: no swallowed ro close anywhere under scripts/ outside the shared lib -------------- #

# A ro remount whose failure is discarded: its stderr sent to /dev/null, or an `|| true` / `|| :` /
# `; true` right after it (past any redirections). Anchored right after the command, so a later
# `2>/dev/null` of ANOTHER command on the same line (a FAIL message reading the unit state) is no hit.
_REDIRS = r"(?:\s*(?:[0-9]|&)?>>?&?\s*[^\s;|&)'\"]+)*?"
_SWALLOW = re.compile(
    r"remount,ro(?:\s+/)?" + _REDIRS + r"\s*(?:\|\|\s*(?:true|:)(?![\w-])|;\s*(?:true|:)(?![\w-]))"
    r"|remount,ro(?:\s+/)?" + _REDIRS + r"\s*(?:2|&)>\s*/dev/null")


def _swallowed_closes():
    hits = []
    for path in sorted((ROOT / "scripts").rglob("*")):
        if not path.is_file() or path == _RO_WINDOW:
            continue
        try:
            text = path.read_text()
        except (UnicodeDecodeError, OSError):
            continue
        text = text.replace("\\\n", " ")  # a continued line is one statement
        for n, line in enumerate(text.splitlines(), 1):
            if line.lstrip().startswith("#"):
                continue
            if _SWALLOW.search(line):
                hits.append(f"{path.relative_to(ROOT)}:{n}: {line.strip()}")
    return hits


def test_no_script_swallows_a_failed_ro_close():
    hits = _swallowed_closes()
    assert hits == [], "a swallowed `remount,ro` outside scripts/lib/ro-window.sh:\n" + "\n".join(hits)


def test_the_sweep_pattern_catches_every_known_swallow_shape():
    for bad in ("(mount -o remount,ro / 2>/dev/null; true)",
                "mount -o remount,ro / 2>/dev/null || true",
                "for _i in 1 2 3; do mount -o remount,ro / 2>/dev/null && break; done",
                "mount -o remount,ro / || :",
                "mount -o remount,ro /; true",
                "mount -o remount,ro / >/dev/null 2>&1 || true",
                "mount -o remount,ro / &>/dev/null"):
        assert _SWALLOW.search(bad), bad
    for good in ('_row_ro_err="$(mount -o remount,ro / 2>&1)" || _row_ro_rc=$?;',
                 'if "$MOUNT" -o remount,ro /; then',
                 "mount -o remount,ro / \\ || fail \"could not remount\"",
                 "ro,relatime,errors=remount-ro",
                 "echo \"the ro remount rc=$rc: $(systemctl is-enabled x 2>/dev/null || true)\"",
                 "echo \"('mount -o remount,ro /' rc=$rc; is-enabled: $(systemctl is-enabled x 2>/dev/null || true))\""):
        assert not _SWALLOW.search(good), good


def test_the_shared_lib_is_sourced_by_every_rw_window_site():
    for rel in ("scripts/deploy-fleet.sh", "scripts/bkshading-deploy-relay.sh",
                "scripts/lib/cam2-painter-ro-persist.sh", "scripts/lib/bkshading-relay-mode.sh",
                "scripts/lib/ndi-discovery.sh", "scripts/lib/dantesync-rollback.sh"):
        text = (ROOT / rel).read_text()
        assert "ro-window.sh" in text and "ro_window_close_cmds" in text, rel


def test_the_lib_parses_and_lints():
    assert subprocess.run(["bash", "-n", str(_RO_WINDOW)]).returncode == 0
