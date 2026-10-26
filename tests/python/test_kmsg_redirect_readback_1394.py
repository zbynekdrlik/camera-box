"""issue 1394: the VT 12 kernel-message redirect is read back from the ioctl BUFFER, never its return value.

Live 26.10.2026 on cam2/cam3/cam5 (kernel 7.0): the redirect worked (an err line written to /dev/kmsg
landed in /dev/vcs12, not /dev/vcs1), yet the setup script read it back as 0 and kept the console level
at 4, and verify-device (ak) failed all three. drivers/tty/vt/vt.c:

    case TIOCL_GETKMSGREDIRECT:
        data = vt_get_kmsg_redirect();
        return put_user(data, p);

so the ioctl returns 0 and the VT is in byte 0 of the argument. The earlier tests ran a fake `perl`
binary and could not see it. These run the REAL perl programs (the setup script's and the verify
gather's) with `CORE::GLOBAL::sysopen` / `ioctl` replaced by a module that models that kernel code.
"""
import os
import re
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "remote-logging.sh"

FAKE_KERNEL = r'''package FakeVtKernel;
# drivers/tty/vt/vt.c tioclinux(): SET (11) stores byte 1 of the argument; GET (17) returns 0 and
# writes the stored VT into byte 0 of the argument (put_user(data, p)).
my $redirect = defined $ENV{FAKE_VT_REDIRECT} ? $ENV{FAKE_VT_REDIRECT} : 0;
BEGIN {
  *CORE::GLOBAL::sysopen = sub { CORE::open($_[0], "<", "/dev/null") };
  *CORE::GLOBAL::ioctl = sub {
    my $sub = ord(substr($_[2], 0, 1));
    if ($sub == 11) { $redirect = ord(substr($_[2], 1, 1)); }
    elsif ($sub == 17) { substr($_[2], 0, 1) = chr($redirect); }
    return "0 but true";
  };
}
1;
'''


def _lib(fn):
    r = subprocess.run(["bash", "-c", '. "$1"; ' + fn, "harness", str(LIB)],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout


def _perl(tmp_path, program, *args, redirect=None):
    (tmp_path / "FakeVtKernel.pm").write_text(FAKE_KERNEL)
    env = dict(os.environ, LC_ALL="C")
    env.pop("FAKE_VT_REDIRECT", None)
    if redirect is not None:
        env["FAKE_VT_REDIRECT"] = str(redirect)
    r = subprocess.run(["perl", "-I", str(tmp_path), "-MFakeVtKernel", "-e", program, *args],
                       capture_output=True, text=True, env=env, timeout=30)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def test_the_setup_redirect_reads_back_the_vt_it_set(tmp_path):
    assert _perl(tmp_path, _lib("remote_log_kmsg_redirect_perl"), "12") == "12"


def test_the_setup_redirect_reads_back_another_vt_as_that_vt(tmp_path):
    assert _perl(tmp_path, _lib("remote_log_kmsg_redirect_perl"), "9") == "9"


def _gather_perl():
    snippet = _lib("remote_log_gather_remote_snippet")
    m = re.search(r"KMSG_REDIRECT=\$\(LC_ALL=C perl -e '([^']*)'", snippet)
    assert m, "the gather reads the redirect through one single-quoted perl program"
    return m.group(1)


def test_the_verify_gather_reads_the_redirect_from_the_buffer(tmp_path):
    assert _perl(tmp_path, _gather_perl(), redirect=12) == "12"


def test_the_verify_gather_reads_no_redirect_as_0(tmp_path):
    assert _perl(tmp_path, _gather_perl(), redirect=0) == "0"
