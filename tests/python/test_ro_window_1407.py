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


def test_a_writer_under_a_numeric_uid_is_named_never_read_as_none(box):
    # fuser prints an unresolvable USER as a number; the ACCESS field is found by its own shape, so
    # `1000 4242 F.... cmd` still names PID 4242 (review round 1).
    (box["stub"] / "fuser").unlink()
    (box["stub"] / "fuser").write_text(
        f"#!{sys.executable}\nimport sys\n"
        "sys.stderr.write('                     USER        PID ACCESS COMMAND\\n')\n"
        "sys.stderr.write('/:                   root     kernel mount /\\n')\n"
        "sys.stderr.write('                     1000      4242 F.... uid-writer\\n')\n"
        "sys.stderr.write('                     root      5151 F.... root-writer\\n')\n")
    (box["stub"] / "fuser").chmod(0o755)
    proc = run_text(box, _close_text(), FAKE_RO_FAIL="1")
    assert proc.returncode == 1
    assert "uid-writer" in proc.stderr and "root-writer" in proc.stderr, proc.stderr
    assert "(none: no process holds a file open for writing on /)" not in proc.stderr, proc.stderr
    out = build(f'. "{_RO_WINDOW}"\nro_window_holders "$1"', proc.stderr).strip()
    assert out.startswith("uid-writer[4242]; root-writer[5151]"), out


def _build_with_stderr(*args):
    proc = subprocess.run(["/bin/bash", "-c", f'set -euo pipefail\n. "{_RO_WINDOW}"\nro_window_close_cmds "$@"',
                           "harness", *args], env={"PATH": "/usr/bin:/bin"}, capture_output=True, text=True)
    assert proc.returncode == 0, proc.stderr
    return proc.stdout, proc.stderr


@pytest.mark.parametrize("bad,clean", [('b"x', "bx"), ("b`id`x", "bidx")])
def test_an_argument_with_a_quote_or_backtick_is_stripped_and_the_close_still_runs(box, bad, clean):
    # review round 2: refusing the close left the root WRITABLE and the dev1 side misread it; the
    # characters are stripped (with a WARNING on dev1) and the full verified close still runs.
    text, err = _build_with_stderr("t", bad, "c", "h")
    assert "WARNING" in err and "ro_window_close_cmds" in err, err
    assert '"' not in text.split("FAIL: [t] ")[1].split("'s root")[0] and "`" not in text, text
    proc = run_text(box, text + "\necho AFTER")
    assert proc.returncode == 0 and "AFTER" in proc.stdout, proc.stderr
    assert root(box) == "ro", log(box)
    busy = box["state"].parent / "busy"
    busy.mkdir()
    bbox = make_box(busy, root="rw")
    proc = run_text(bbox, text, FAKE_RO_FAIL="1")
    assert proc.returncode == 1
    assert f"FAIL: [t] {clean}'s root is NOT read-only" in proc.stderr, proc.stderr
    rc = build(f'. "{_RO_WINDOW}"\nif ro_window_close_failed "$1"; then echo yes; else echo no; fi', proc.stderr)
    assert rc.strip() == "yes"


def test_close_failed_predicate_reads_the_shared_fail_line(box):
    proc = run_text(box, _close_text(), FAKE_RO_FAIL="1")
    rc = build(f'. "{_RO_WINDOW}"\nif ro_window_close_failed "$1"; then echo yes; else echo no; fi', proc.stderr)
    assert rc.strip() == "yes"
    rc = build(f'. "{_RO_WINDOW}"\nif ro_window_close_failed "$1"; then echo yes; else echo no; fi',
               "SELF-HEAL: restored previous dantesync binary")
    assert rc.strip() == "no"


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
    # ONE first-token reading of the root mode (review round 1): the `ro | ro,*` pattern appears only
    # inside an emitted ro_root_mount_mode definition, never as an inline copy at the site.
    assert text.count("ro | ro,*") == text.count("ro_root_mount_mode () "), (
        f"{site}: an inline copy of the first-token root-mode reading:\n{text}")


# ---- the sweep: no swallowed ro close anywhere under scripts/ outside the shared lib -------------- #

# A ro remount whose failure is discarded: its stderr sent to /dev/null, or an `|| true` / `|| :` /
# `; true` right after it (past any redirections). Anchored right after the command, so a later
# `2>/dev/null` of ANOTHER command on the same line (a FAIL message reading the unit state) is no hit.
_RO_CALL = re.compile(r"(?:remount,ro|ro,remount)(?:,[\w=-]+)*[\"']?(?:\s+/(?=[\s;|&)'\"]|$))?")
_REDIRS = re.compile(r"(?:\s*(?:[0-9]|&)?>>?&?\s*[^\s;|&)'\"]+)*")
_DISCARDS = (re.compile(r"(?:2|&)>>?\s*/dev/null"), re.compile(r">\s*/dev/null.*2>&1"))
# what may follow an ro remount without checking it: an `||` that goes on (true, :, a message, a
# zero return/exit, continue/break), `; true`, `; :`, or a retry loop's `&& break`. A `|| { ... }`
# group is read separately (_group_goes_on): to its MATCHING brace, so a `${var}` inside it never
# ends it early.
_GOES_ON = re.compile(r"\s*(?:\|\|\s*(?:true|:|echo|printf|warn|log|info|err|logger|return\s+0|exit\s+0|continue|break)"
                      r"(?![\w-])|;\s*(?:true|:)(?![\w-])|&&\s*break(?![\w-]))")
_GROUP = re.compile(r"\s*\|\|\s*\{")
# a group is loud when it ends the step with a failure: a non-zero exit/return, a bare return
# (it passes the mount's failure on), fail or die.
_LOUD = re.compile(r"\b(?:exit|return)\s+[1-9]|\breturn\s*(?:;|$|\})|\b(?:fail|die)\b")


def _group_goes_on(after):
    m = _GROUP.match(after)
    if not m:
        return False
    depth, i = 1, m.end()
    while i < len(after) and depth:
        if after[i] == "{":
            depth += 1
        elif after[i] == "}":
            depth -= 1
        i += 1
    return not _LOUD.search(after[m.end():i - 1])


class _Sweep:
    """A ro remount whose failure is discarded: its error sent to /dev/null, or an `||` / `;` /
    `&& break` right after it (past its redirections) that just goes on. Anchored right after the
    command, so a FAIL message that reads unit state with `2>/dev/null || true` later on the same
    line is no hit, and a loud `|| fail ...` / `|| { ...; exit 1; }` is no hit either."""

    @staticmethod
    def search(line):
        for m in _RO_CALL.finditer(line):
            rest = line[m.end():]
            redirs = _REDIRS.match(rest).group(0)
            if any(d.search(redirs) for d in _DISCARDS):
                return True
            if _GOES_ON.match(rest[len(redirs):]) or _group_goes_on(rest[len(redirs):]):
                return True
        return False


_SWALLOW = _Sweep


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
                "mount -o remount,ro / &>/dev/null",
                # review round 1: the shapes the first pattern missed
                "mount -o remount,ro / || echo \"WARNING: could not remount\"",
                "mount -o remount,ro / || return 0",
                "mount -o remount,ro / || exit 0",
                "mount -o ro,remount / || true",
                "mount -o \"remount,ro\" / 2>/dev/null",
                "mount -o remount,ro,noatime / || true",
                "mount -o remount,ro / >/dev/null 2>&1",
                "for i in 1 2 3; do mount -o remount,ro / && break; sleep 2; done",
                # review round 2: print-only helpers and a message-only brace group go on too
                "mount -o remount,ro / || err \"could not remount\"",
                "mount -o remount,ro / || { warn \"busy\"; }",
                "mount -o remount,ro / || logger -t deploy busy",
                # review round 3: a group that ends in a ZERO return/exit goes on too
                "mount -o remount,ro / || { echo x; return 0; }",
                "mount -o remount,ro / || { warn x; exit 0; }",
                # review round 4: inside a group a bare return/exit (or $?) hands back the LAST
                # command's status, not the mount's
                "mount -o remount,ro / || { warn busy; return; }",
                "mount -o remount,ro / || { echo x; exit $?; }"):
        assert _SWALLOW.search(bad), bad
    for good in ('_row_ro_err="$(mount -o remount,ro / 2>&1)" || _row_ro_rc=$?;',
                 'if "$MOUNT" -o remount,ro /; then',
                 "mount -o remount,ro / \\ || fail \"could not remount\"",
                 "ro,relatime,errors=remount-ro",
                 "echo \"the ro remount rc=$rc: $(systemctl is-enabled x 2>/dev/null || true)\"",
                 "echo \"('mount -o remount,ro /' rc=$rc; is-enabled: $(systemctl is-enabled x 2>/dev/null || true))\"",
                 "mount -o remount,ro / || fail \"could not remount root back to read-only\"",
                 "mount -o remount,ro / || { echo \"FAIL: busy\" >&2; exit 1; }",
                 # review round 2: a bare / non-zero return passes the failure on
                 "mount -o remount,ro / || return",
                 "mount -o remount,ro / || return 1",
                 # review round 3: a loud group that names a ${var} before its exit
                 "mount -o remount,ro / || { echo \"FAIL on ${host}\" >&2; exit 1; }",
                 "mount -o remount,ro / || { err \"x ${h}\"; return 1; }",
                 # review round 4: a variable exit code, a quoted brace
                 "mount -o remount,ro / || { rc=$?; echo \"FAIL rc=$rc\" >&2; exit \"$rc\"; }",
                 "mount -o remount,ro / || { echo \"}\"; exit 1; }",
                 "printf 'mount -o remount,rw / && apt-get update && mount -o remount,ro /   # comment'"):
        assert not _SWALLOW.search(good), good


def test_the_sweep_reads_a_group_across_lines():
    # review round 4: the sweep walks lines, so a loud group whose body sits on the next lines must
    # be read to its matching brace, never as an empty (swallowing) group; an unclosed one is no hit.
    loud = "mount -o remount,ro / || {\n  echo \"FAIL: busy\" >&2\n  exit 1\n}\nnext_step\n"
    quiet = "mount -o remount,ro / || {\n  echo busy\n}\nnext_step\n"
    assert _swallowed_in_text(loud) == [], loud
    assert len(_swallowed_in_text(quiet)) == 1, quiet
    assert _swallowed_in_text("mount -o remount,ro / || {\n  echo x\n") == [], "an unclosed group is no hit"


def test_deploy_fleet_names_a_pending_painter_start_only_for_an_enable_now_restore():
    # review round 3: the interrupt advice "start it there by hand" must never be printed for a
    # deliberately dark (#892 EVENT) painter -- a start would put the QR on a live broadcast.
    text = (ROOT / "scripts" / "deploy-fleet.sh").read_text()
    start = text.index("\ndeploy_frame_probe_to_painter() {\n")
    body = text[start:text.index("\n}\n", start)]
    marks = [ln for ln in body.splitlines() if re.search(r"\bPENDING_START=\"\$painter", ln)]
    assert marks, "the painter swap sets a pending start"
    assert all("enable-now" in ln for ln in marks), marks


def test_the_shared_lib_is_sourced_by_every_rw_window_site():
    for rel in ("scripts/deploy-fleet.sh", "scripts/bkshading-deploy-relay.sh",
                "scripts/lib/cam2-painter-ro-persist.sh", "scripts/lib/bkshading-relay-mode.sh",
                "scripts/lib/ndi-discovery.sh", "scripts/lib/dantesync-rollback.sh"):
        text = (ROOT / rel).read_text()
        assert "ro-window.sh" in text and "ro_window_close_cmds" in text, rel


def test_the_lib_parses_and_lints():
    assert subprocess.run(["bash", "-n", str(_RO_WINDOW)]).returncode == 0
