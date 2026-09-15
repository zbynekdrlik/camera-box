"""Tier-0 tests for two live defects found bringing cam2's off-box logging up on 14.9.2026 (#1311):

D9 -- `remote_log_gather_remote_snippet`'s JU_STATE_SAVE extraction greps the FIRST `--save-state=`
      occurrence in the journal-upload drop-in dir; the shipped drop-in's own COMMENT names the stock
      default `--save-state=/var/lib/...` BEFORE the real `ExecStart=` line, so verify-device read the
      comment and failed check (ak) on a correctly provisioned box (the self-collision anchor class).
D8 -- `dev1-remote-log-install.sh` creates /var/log/cambox root:root 0755 while rsyslog drops to
      syslog:syslog -> `open error: Permission denied`, the per-box kernel file was never written.
"""

import os
import re
import subprocess
import tempfile
import unittest

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "remote-logging.sh")
INSTALLER = os.path.join(REPO, "scripts", "dev1-remote-log-install.sh")


def _bash(script: str) -> subprocess.CompletedProcess:
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True, timeout=30, cwd=REPO)


class JournalUploadStateGather(unittest.TestCase):
    def test_gather_reads_the_execstart_save_state_not_the_dropin_comment(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = os.path.join(tmp, "systemd-journal-upload.service.d")
            os.makedirs(d)
            r = _bash(f'set -u; . "{LIB}"; remote_log_journal_upload_dropin_content')
            self.assertEqual(r.returncode, 0, r.stderr)
            dropin = r.stdout
            self.assertIn("--save-state=/var/lib", dropin, "fixture premise: the drop-in comment names the stock default")
            self.assertRegex(dropin, r"(?m)^ExecStart=.*--save-state=/run/")
            with open(os.path.join(d, "10-cambox-rostate.conf"), "w") as fh:
                fh.write(dropin)
            r = _bash(f'set -u; . "{LIB}"; remote_log_gather_remote_snippet')
            self.assertEqual(r.returncode, 0, r.stderr)
            line = [l for l in r.stdout.splitlines() if l.startswith('echo "JU_STATE_SAVE=')]
            self.assertEqual(len(line), 1, r.stdout)
            cmd = line[0].replace("/etc/systemd/system/systemd-journal-upload.service.d", d)
            out = _bash(cmd).stdout.strip()
            self.assertEqual(out, "JU_STATE_SAVE=--save-state=/run/systemd/journal-upload/state", out)


class Dev1SinkDirOwnership(unittest.TestCase):
    def test_installer_creates_the_sink_dir_owned_by_rsyslogs_dropped_user(self):
        text = open(INSTALLER).read()
        lines = [l for l in text.splitlines() if re.search(r"install -d .*CAMBOX_LOG_DIR", l)]
        self.assertTrue(lines, "the installer must create the sink dir")
        for l in lines:
            self.assertIn("-o syslog -g adm", l, f"sink dir must be owned by rsyslog's PrivDropToUser (got: {l.strip()})")
        r = _bash(f'bash "{INSTALLER}"')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("install -d -m 0755 -o syslog -g adm /var/log/cambox", r.stdout)


if __name__ == "__main__":
    unittest.main()
