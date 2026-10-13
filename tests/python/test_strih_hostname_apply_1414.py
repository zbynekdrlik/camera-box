"""issue 1414 (live strih-snv rename, 13.10.2026): setup-strih's hostname step must also rewrite the
127.0.1.1 line of /etc/hosts (it still read the pre-rename name) and restart avahi-daemon, whose mDNS host
name kept the old `<name>.local` until it was restarted by hand. Only on a real change; never on a box whose
name is already right."""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
FACTS = REPO / "scripts" / "lib" / "strih-box-facts.sh"
PROVISION = REPO / "scripts" / "lib" / "strih-provision.sh"


def _run(tmp_path, current, hosts_text):
    b = tmp_path / "bin"
    b.mkdir()
    log = tmp_path / "calls"
    (b / "hostname").write_text("#!/bin/bash\necho %s\n" % current)
    (b / "hostnamectl").write_text('#!/bin/bash\necho "hostnamectl $*" >> %s\n' % log)
    (b / "systemctl").write_text('#!/bin/bash\necho "systemctl $*" >> %s\n' % log)
    for f in b.iterdir():
        f.chmod(0o755)
    hosts = tmp_path / "hosts"
    hosts.write_text(hosts_text)
    env = dict(os.environ, PATH="%s:%s" % (b, os.environ["PATH"]))
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. "$1"\nstrih_box_load strih-snv\n. "$2"\n'
                        'strih_hostname_apply strih-snv "$3"', "h", str(FACTS), str(PROVISION), str(hosts)],
                       capture_output=True, text=True, env=env, timeout=60)
    return r, hosts.read_text(), (log.read_text() if log.exists() else "")


def test_a_rename_rewrites_the_hosts_line_and_restarts_avahi(tmp_path):
    r, hosts, calls = _run(tmp_path, "old-box", "127.0.0.1\tlocalhost\n127.0.1.1\told-box-new\n::1 ip6-localhost\n")
    assert r.returncode == 0, r.stderr
    assert "127.0.1.1\tstrih-snv\n" in hosts and "old-box-new" not in hosts
    assert "127.0.0.1\tlocalhost" in hosts and "::1 ip6-localhost" in hosts
    assert "hostnamectl set-hostname strih-snv" in calls
    assert "systemctl try-restart avahi-daemon" in calls


def test_a_missing_hosts_line_is_added(tmp_path):
    r, hosts, _ = _run(tmp_path, "old-box", "127.0.0.1\tlocalhost\n")
    assert r.returncode == 0, r.stderr
    assert hosts.endswith("127.0.1.1\tstrih-snv\n")


def test_a_box_already_named_right_changes_nothing(tmp_path):
    r, hosts, calls = _run(tmp_path, "strih-snv", "127.0.0.1\tlocalhost\n127.0.1.1\tstrih-snv\n")
    assert r.returncode == 0, r.stderr
    assert calls == "" and hosts == "127.0.0.1\tlocalhost\n127.0.1.1\tstrih-snv\n"


def test_setup_calls_it():
    s = (REPO / "scripts" / "setup-strih.sh").read_text()
    assert 'strih_hostname_apply "$(strih_lx_hostname)" /etc/hosts' in s
    assert "hostnamectl set-hostname" not in s
