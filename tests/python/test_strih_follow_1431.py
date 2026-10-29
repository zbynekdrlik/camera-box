"""Issue 1431 (joint event: `strih.lan` is the ONE switch): verify-device `(ax)` names the strih a cambox follows.

The cambox's intercom target, dantesync NTP server, shading registration and cameraman preview all follow whatever
`strih.lan` resolves to ON THE BOX. `(ax)` resolves it there (`getent ahostsv4`), and passes only when the address is
the STRIH_IP of a strih-role fact file (scripts/strih-boxes/*.env, parsed, never sourced), naming that box; anything
else (no answer, a stream box, an unknown address) FAILs naming the address.
"""
import os
import shutil
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
LIB = os.path.join(REPO, "scripts", "lib", "strih-follow.sh")
VERIFY = os.path.join(REPO, "scripts", "verify-device.sh")
BOXES = os.path.join(REPO, "scripts", "strih-boxes")


def _read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def _lib(snippet, *args, env=None):
    e = {k: v for k, v in os.environ.items() if not k.startswith("STRIH_")}
    e.update(env or {})
    src = 'set -euo pipefail\n. "%s"\n%s' % (LIB, snippet)
    return subprocess.run(["bash", "-c", src, "harness", *args], capture_output=True, text=True, env=e)


def test_the_strih_rows_are_the_strih_role_fact_files_only():
    r = _lib("strih_follow_strih_rows")
    assert r.returncode == 0, r.stderr
    rows = sorted(r.stdout.splitlines())
    assert rows == ["strih-pp\t10.77.8.202", "strih-snv\t10.77.9.202"], rows


def test_the_verdict_names_the_followed_strih():
    for ip, box in (("10.77.9.202", "strih-snv"), ("10.77.8.202", "strih-pp")):
        r = _lib('strih_follow_verdict "$1"', ip)
        assert r.returncode == 0 and r.stdout.strip() == "ok\t%s\t%s" % (box, ip), (ip, r.stdout, r.stderr)


def test_the_verdict_fails_an_address_that_is_no_strih_and_names_it():
    for ip in ("10.77.9.204", "10.77.9.203", "192.0.2.7"):  # a stream box, the old notebook, a stranger
        out = _lib('strih_follow_verdict "$1"', ip).stdout
        assert out.startswith("FAIL") and ip in out, (ip, out)
        assert "10.77.9.202" in out and "10.77.8.202" in out, "the known strih addresses are named: " + out
    none = _lib('strih_follow_verdict ""').stdout
    assert none.startswith("FAIL") and "does not resolve" in none, none
    junk = _lib('strih_follow_verdict "$1"', "not-an-ip").stdout
    assert junk.startswith("FAIL"), junk


def test_a_fact_file_that_does_not_parse_fails_closed():
    with tempfile.TemporaryDirectory() as tmp:
        for f in os.listdir(BOXES):
            shutil.copy(os.path.join(BOXES, f), tmp)
        with open(os.path.join(tmp, "strih-pp.env"), "a") as f:
            f.write("NOT A FACT\n")
        r = _lib('strih_follow_verdict "$1"', "10.77.9.202", env={"STRIH_BOXES_DIR": tmp})
        assert r.stdout.startswith("FAIL") and "strih-pp.env" in r.stdout, r.stdout + r.stderr


def test_the_gather_resolves_strih_lan_on_the_box_and_prints_the_first_ipv4():
    snip = _lib("strih_follow_gather_remote_snippet").stdout
    assert "getent ahostsv4 strih.lan" in snip, snip
    with tempfile.TemporaryDirectory() as tmp:
        getent = os.path.join(tmp, "getent")
        with open(getent, "w") as f:
            f.write('#!/bin/sh\nprintf "10.77.8.202     STREAM strih.lan\\n10.77.8.202     DGRAM\\n"\n')
        os.chmod(getent, 0o755)
        out = subprocess.run(["sh", "-c", snip], capture_output=True, text=True,
                             env=dict(os.environ, PATH=tmp + ":" + os.environ["PATH"])).stdout
        assert out.strip() == "10.77.8.202", out
        with open(getent, "w") as f:
            f.write("#!/bin/sh\nexit 2\n")
        out = subprocess.run(["sh", "-c", snip], capture_output=True, text=True,
                             env=dict(os.environ, PATH=tmp + ":" + os.environ["PATH"]))
        assert out.returncode == 0 and out.stdout.strip() == "", out


def _run_ax(remote_out, rc=0):
    """Execute the REAL (ax) block sliced from verify-device.sh with ssh_box stubbed."""
    s = _read(VERIFY)
    i = s.find("\n# (ax) ")
    assert i >= 0, "verify-device.sh has no (ax) block"
    block = s[i:s.find("\n# (", i + 1)]
    assert 'warn "' not in block, "(ax) FAILs, never only warns"
    prelude = ('ok() { echo "OK $1"; }\nfail() { echo "FAIL $1"; }\n'
               'ssh_box() { printf "%s" "$AX_OUT"; return "$AX_RC"; }\n')
    src = 'set -euo pipefail\n. "%s"\n%s%s' % (LIB, prelude, block)
    e = {k: v for k, v in os.environ.items() if not k.startswith("STRIH_")}
    e.update(AX_OUT=remote_out, AX_RC=str(rc))
    return subprocess.run(["bash", "-c", src], capture_output=True, text=True, env=e)


def test_the_ax_block_passes_a_box_following_a_strih_and_fails_otherwise():
    ok = _run_ax("10.77.8.202\n")
    assert ok.returncode == 0, ok.stderr
    assert ok.stdout.startswith("OK ") and "strih-pp" in ok.stdout and "10.77.8.202" in ok.stdout, ok.stdout
    bad = _run_ax("10.77.9.204\n")
    assert bad.returncode == 0, bad.stderr
    assert bad.stdout.startswith("FAIL ") and "10.77.9.204" in bad.stdout, bad.stdout
    empty = _run_ax("")
    assert empty.stdout.startswith("FAIL ") and "does not resolve" in empty.stdout, empty.stdout
    unreadable = _run_ax("", rc=255)
    assert unreadable.stdout.startswith("FAIL ") and "ssh" in unreadable.stdout, unreadable.stdout


def test_ax_is_documented_three_times_and_sits_before_q():
    s = _read(VERIFY)
    head = s[: s.find("set -euo pipefail")]
    assert "#   (ax) " in head, "document (ax) in the header Checks list"
    usage = s[s.find("usage()"):]
    assert "(ax)" in usage[: usage.find("\nEOF")], "document (ax) in usage()"
    i_ax = s.find("\n# (ax) ")
    assert 0 <= i_ax < s.find("\n# (q) .bak cruft drift"), "(ax) must sit before (q)"
    # outside the slices other tests EXECUTE: after (ak), before (aw)
    assert s.find("\n# (ak) ") < i_ax < s.find("\n# (aw) ")
