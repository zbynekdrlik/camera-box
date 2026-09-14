"""Tier-0: the imag projector-vsync OBS-log gather must be BOUNDED (issue 1151 / the #1222 discipline).

Live 14.9.2026: imag's current OBS session log had grown to 1.2 GB (4.5 M lines since 1.9.); the
E2E `[0/8]` marker check `cat`'d the WHOLE file over ssh into a bash variable and ran the verdict on
it -> bash died with SIGSEGV (exit 139) and the release E2E aborted before `[1/8]`. The verdict only
needs the `projector-vsync:` marker lines, so the remote snippet must ship only those, capped.
"""

import os
import subprocess
import tempfile
import unittest

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "obs-projector-vsync.sh")
ARMED = "15:52:14.820: projector-vsync: present-vsync ARMED (GL/EGL swap interval 1; no-op on D3D11)"
CLEARED = "12:00:00.000: projector-vsync: present-vsync cleared (GL/EGL swap interval 0; no-op on D3D11)"


def _snippet() -> str:
    r = subprocess.run(["bash", "-c", f'set -u; . "{LIB}"; projector_vsync_gather_remote_snippet'],
                       capture_output=True, text=True, timeout=20)
    assert r.returncode == 0, r.stderr
    return r.stdout


class BoundedGather(unittest.TestCase):
    def test_snippet_never_cats_the_whole_log(self):
        s = _snippet()
        self.assertNotIn('cat "$f"', s, "the gather must not ship the whole OBS log (1.2 GB live)")
        self.assertIn("projector-vsync", s, "the gather must filter to the marker family it grades")
        self.assertIn("tail -n", s, "the gather must cap its output")

    def test_big_log_yields_bounded_output_and_the_right_verdict(self):
        with tempfile.TemporaryDirectory() as home:
            d = os.path.join(home, ".config", "obs-studio", "logs")
            os.makedirs(d)
            big = os.path.join(d, "2026-09-01 16-01-00.txt")
            with open(big, "w") as fh:
                fh.write(CLEARED + "\n")
                for i in range(300_000):
                    fh.write(f"12:00:{i % 60:02d}.000: genlock-fifo audit 'NDI cam{i % 7 + 1}': received={i} noise\n")
                fh.write(ARMED + "\n")
                for i in range(1_000):
                    fh.write("12:00:59.000: program-render-audit: render_fps=60.0 lagged=0\n")
            r = subprocess.run(["bash", "-c", _snippet()], capture_output=True, text=True, timeout=60,
                               env={**os.environ, "HOME": home})
            self.assertEqual(r.returncode, 0, r.stderr)
            lines = r.stdout.splitlines()
            self.assertLessEqual(len(lines), 200, f"gather shipped {len(lines)} lines — unbounded")
            self.assertIn(ARMED, lines)
            v = subprocess.run(["bash", "-c", f'set -u; . "{LIB}"; projector_vsync_verdict "$1"', "_", r.stdout],
                               capture_output=True, text=True, timeout=20)
            self.assertTrue(v.stdout.startswith("projector_vsync|OK|"), v.stdout)


if __name__ == "__main__":
    unittest.main()
