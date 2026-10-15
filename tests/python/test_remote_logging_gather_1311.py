"""Tier-0 tests for two live defects found bringing cam2's off-box logging up on 14.9.2026 (#1311):

D9 -- `remote_log_gather_remote_snippet`'s JU_STATE_SAVE extraction greps the FIRST `--save-state=`
      occurrence in the journal-upload drop-in dir; the shipped drop-in's own COMMENT names the stock
      default `--save-state=/var/lib/...` BEFORE the real `ExecStart=` line, so verify-device read the
      comment and failed check (ak) on a correctly provisioned box (the self-collision anchor class).
(D8, the dev1 receiver's sink-dir ownership, left with the dev1 installer in issue 1394: the receiver
is the venue strih now, scripts/lib/strih-cambox-log-sink.sh: test_strih_cambox_log_sink_1394.py
pins its User=syslog + LogsDirectory=cambox + `su syslog syslog` rotation.)
"""

import os
import subprocess
import tempfile
import unittest

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "remote-logging.sh")


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


if __name__ == "__main__":
    unittest.main()
