"""issue 1462 -- RustDesk is required on every Linux workstation box.

Owner 25.10.2026 (verbatim): "preco na stream-snv nebezi rustdesk!!!!" and "vsade na rustdeskoch mame rovnake
heslo". stream-snv was handed over without RustDesk: setup step 16b only WARNed without its password file and
verify item 29 only NOTEd "not installed". These tests pin the state reader, the item-29 row (every missing
piece FAILs), the step-16b wiring (a missing password fails setup) and the dev1 apply script (the value on
stdin only, never in an argv or the output).
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
APPLY = SCRIPTS / "rustdesk-password-apply.sh"
TOML_SET = "rendezvous_server = 'rs-ny.rustdesk.com:21116'\npassword = '00enc+value=='\nsalt = 'x'\n"
TOML_EMPTY = "rendezvous_server = 'rs-ny.rustdesk.com:21116'\npassword = ''\nsalt = 'x'\n"


def _exec(path, text):
    path.write_text(text)
    path.chmod(0o755)
    return path


class Box:
    """A stub PATH: rustdesk (--get-id prints state/id), systemctl (is-active prints state/active)."""

    def __init__(self, tmp, rustdesk=True, active="active", toml=TOML_SET, rd_id="1286350194"):
        self.bin = tmp / "bin"
        self.bin.mkdir()
        self.conf = tmp / "RustDesk.toml"
        if toml is not None:
            self.conf.write_text(toml)
        if rustdesk:
            _exec(self.bin / "rustdesk", '#!/bin/bash\n[ "$1" = --get-id ] && printf "%s\\n" "%s"\nexit 0\n' % ("%s", rd_id))
        _exec(self.bin / "systemctl", '#!/bin/bash\ncase "$*" in *is-active*) echo %s; [ %s = active ];; esac\n'
              % (active, active))
        # never a real sudo: an unreadable config must read as "no password"
        _exec(self.bin / "sudo", "#!/bin/bash\nexit 1\n")

    def run(self, body):
        script = 'set -euo pipefail\n. "$1"\nstrih_box_load stream-snv\n. "$2"\n' + body
        env = {"PATH": "%s:/usr/bin:/bin" % self.bin, "HOME": os.environ.get("HOME", "/tmp"),
               "STRIH_RUSTDESK_CONF": str(self.conf)}
        return subprocess.run(["bash", "-c", script, "h", str(FACTS), str(PROVISION)],
                              capture_output=True, text=True, env=env, timeout=60)


def test_the_state_reads_each_case(tmp_path):
    cases = [(dict(rustdesk=False), "absent"), (dict(active="inactive"), "down"),
             (dict(toml=TOML_EMPTY), "nopw"), (dict(toml=None), "nopw"), (dict(), "ready")]
    for i, (kw, want) in enumerate(cases):
        d = tmp_path / str(i)
        d.mkdir()
        r = Box(d, **kw).run("strih_rustdesk_state")
        assert r.returncode == 0 and r.stdout == want, (kw, r.stdout, r.stderr)


def test_the_state_never_prints_the_stored_value(tmp_path):
    r = Box(tmp_path).run("strih_rustdesk_state; strih_rustdesk_grade_row stream-snv")
    assert "00enc" not in r.stdout + r.stderr


def _row(tmp_path, **kw):
    r = Box(tmp_path, **kw).run("strih_rustdesk_grade_row stream-snv")
    assert r.returncode == 0, r.stderr
    st, text = r.stdout.strip().split("|", 1)
    return st, text


def test_a_box_without_rustdesk_fails_item_29_never_a_note(tmp_path):
    st, text = _row(tmp_path, rustdesk=False)
    assert st == "FAIL" and "not installed" in text and "16b" in text


def test_no_permanent_password_fails_and_names_the_dev1_apply_command(tmp_path):
    st, text = _row(tmp_path, toml=TOML_EMPTY)
    assert st == "FAIL" and "no permanent password" in text
    assert "secret exec" in text and "scripts/rustdesk-password-apply.sh stream-snv" in text


def test_an_inactive_service_fails(tmp_path):
    st, text = _row(tmp_path, active="failed")
    assert st == "FAIL" and "failed" in text


def test_a_ready_box_passes_with_its_connect_id(tmp_path):
    st, text = _row(tmp_path)
    assert st == "OK" and "1286350194" in text


def test_a_ready_box_without_a_connect_id_fails(tmp_path):
    st, _ = _row(tmp_path, rd_id="")
    assert st == "FAIL"


def _block(path, start, end):
    s = path.read_text()
    i = s.index(start)
    return s[i:s.index(end, i)]


def test_verify_item_29_is_the_lib_row_and_has_no_not_installed_note():
    item = _block(SCRIPTS / "verify-strih.sh", "# 29) RustDesk", "\n\n")
    assert 'strih_rustdesk_grade_row "$(hostname -s)"' in item
    assert "note " not in item and "report-only" not in item


def test_step_16b_fails_without_a_password_and_never_only_warns():
    step = _block(SCRIPTS / "setup-strih.sh", 'step "16b"', 'step "16c"')
    assert "strih_rustdesk_state" in step
    assert "RustDesk NOT installed" not in step and "warn " not in step
    # without a password: the package installed, then setup fails naming the apply command
    tail = step[step.index("else\n  if"):]
    assert "strih_rustdesk_package_cmds" in tail
    assert tail.index("strih_rustdesk_package_cmds") < tail.index('fail "issue 1462')
    assert "strih_rustdesk_apply_hint" in tail
    # a ready box is left alone: no install, no password call
    ready = step[step.index('if [ "$RD_STATE" = ready ]'):step.index("elif")]
    assert "install_cmds" not in ready and "package_cmds" not in ready


def test_the_package_emitter_never_sets_a_password_and_parses():
    r = subprocess.run(["bash", "-c", '. "$1"; strih_rustdesk_package_cmds 1.4.9 http://x/rd.deb ABC', "h", str(PROVISION)],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    assert "--password" not in r.stdout and "systemctl enable --now rustdesk" in r.stdout
    assert subprocess.run(["bash", "-n"], input=r.stdout, text=True).returncode == 0


def _apply_env(tmp_path, fail_box=None):
    """A fake sshpass that records its argv and runs the remote text locally against a fake sudo/rustdesk."""
    b = tmp_path / "bin"
    b.mkdir()
    log, got = tmp_path / "argv.log", tmp_path / "got"
    _exec(b / "sshpass", '#!/bin/bash\nprintf "%%s\\n" "$*" >> %s\nremote="${@: -1}"\nhost="${@: -2:1}"\n'
          '[ "$host" = "newlevel@%s" ] && exit 255\nexec bash -c "$remote"\n' % (log, fail_box or "-"))
    _exec(b / "sudo", '#!/bin/bash\n[ "$1" = -n ] && shift\nexec "$@"\n')
    _exec(b / "rustdesk", '#!/bin/bash\n[ "$1" = --password ] && printf "%%s" "$2" > %s && echo "Done! $2"\n' % got)
    _exec(b / "resolve", "#!/bin/bash\n[ \"$1\" = known ]\n")
    env = {"PATH": "%s:/usr/bin:/bin" % b, "RUSTDESK_PERMANENT_PASSWORD": "s3cret value'x",
           "RUSTDESK_APPLY_SSHPASS": str(b / "sshpass"), "RUSTDESK_APPLY_RESOLVE": str(b / "resolve")}
    return env, log, got


def test_apply_hands_the_value_on_stdin_only(tmp_path):
    env, log, got = _apply_env(tmp_path)
    r = subprocess.run(["bash", str(APPLY), "stream-snv"], capture_output=True, text=True, env=env, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    assert got.read_text() == "s3cret value'x"           # the box got the exact value
    assert "s3cret" not in log.read_text()               # never in an argv
    assert "s3cret" not in r.stdout + r.stderr           # never in the output, rustdesk's own echo discarded
    assert r.stdout.strip() == "stream-snv: applied (stream-snv.lan)"


def test_apply_reports_each_box_and_fails_when_one_fails(tmp_path):
    env, _, _ = _apply_env(tmp_path, fail_box="strih-pp.lan")
    r = subprocess.run(["bash", str(APPLY), "known", "strih-pp"], capture_output=True, text=True, env=env, timeout=60)
    assert r.returncode == 1
    lines = r.stdout.splitlines()
    assert lines[0] == "known: applied (known)"         # a name that resolves is used as it is
    assert lines[1].startswith("strih-pp: FAILED (strih-pp.lan")


def test_apply_refuses_without_the_credential_channel():
    env = {"PATH": "/usr/bin:/bin"}
    r = subprocess.run(["bash", str(APPLY), "stream-snv"], capture_output=True, text=True, env=env, timeout=30)
    assert r.returncode != 0 and "secret exec" in r.stderr
