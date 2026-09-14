"""Tier-0: drift-guard's imag OBS-log gather must be BOUNDED + marker-filtered (issue 1151).

FOLDED deliverable (net-drain ratchet, 14.9.2026): the E2E `[0/8]` projector-vsync reader was fixed
in 2e788561b (`grep -a <marker> | tail -n 50`); `scripts/drift-guard.sh` `gather_and_check_imag`
still shipped the WHOLE newest `~/.config/obs-studio/logs/*.txt` over ssh into a bash variable
(`cat "$f"`) for its five OBS-log parsers. On imag's live 1.2 GB / 4.5 M-line session log the harness
bash SIGSEGVs (exit 139) -> `--check-imag` exits 139 -> `rig-mode.sh test` HARD-BLOCKS at the
issue-789 TEST-entry gate. The gather must ship only the marker-family lines the five parsers grade
(union of `genlock:` / `video settings reset:` + `fps:` / `projector-vsync:`), capped, and it is
extracted into a pure `drift_guard_imag_obs_log_gather_snippet` mirroring
`projector_vsync_gather_remote_snippet` so it is unit-testable without ssh.
"""

import os
import subprocess
import tempfile
import unittest

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
SCRIPT = os.path.join(REPO, "scripts", "drift-guard.sh")

# A `.` source of drift-guard.sh stops at its source-guard (BASH_SOURCE != $0) before main, exposing
# the pure functions: the new snippet + the five OBS-log parsers (projector_vsync_verdict comes from
# the obs-projector-vsync.sh lib drift-guard.sh itself sources).
SRC = f'. "{SCRIPT}"'

ADAPTER_FPS = "11:40:39.714: monitor adapter fps: 60/1"
RESET = "11:40:39.714: video settings reset:"
OUT_FPS = "11:40:39.714: \tfps:               30/1"
GENLOCK_CAP = "07:42:29.658: genlock: wall-clock-slaved render tick ENABLED (OBS_GENLOCK_WALL_CLOCK, slew cap 2000000 ns/tick)"
GENLOCK_LAT = "07:42:38.746: genlock: latency = 3 ms (0 frames @ 60.000fps) (OBS_GENLOCK_LATENCY_MS)"
GENLOCK_RT = "14:27:54.427: genlock: render-tick thread set SCHED_FIFO prio 10 on the isolated core (#484)"
PROJECTOR = "15:52:14.820: projector-vsync: present-vsync ARMED (GL/EGL swap interval 1; no-op on D3D11)"
# Bulk families that must NOT survive the union grep (the ~90 MB/day the log is made of):
NOISE_FAMILIES = [
    "12:00:{s:02d}.000: genlock-fifo audit 'NDI cam1': received={i} latency_ms=3",
    "12:00:{s:02d}.000: program-render-audit: render_fps=60.0 lagged=0",
    "12:00:{s:02d}.000: multiview-audit: rendered_fps=60.0",
]


def _bash(body: str, home=None, timeout=90) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    if home is not None:
        env["HOME"] = home
    return subprocess.run(["bash", "-c", body], capture_output=True, text=True, timeout=timeout, env=env)


def _snippet() -> str:
    r = _bash(f'{SRC}; drift_guard_imag_obs_log_gather_snippet', timeout=30)
    assert r.returncode == 0, f"snippet call failed: {r.stderr}"
    return r.stdout


def _write_log(home: str, lines) -> str:
    d = os.path.join(home, ".config", "obs-studio", "logs")
    os.makedirs(d)
    p = os.path.join(d, "2026-09-01 19-34-00.txt")
    with open(p, "w") as fh:
        for ln in lines:
            fh.write(ln + "\n")
    return p


def _parser(gathered: str, call: str) -> str:
    # Source drift-guard.sh and run one parser over the gathered text passed as a positional arg
    # ($1, never an env value — a large value would blow ARG_MAX at spawn; a shell-function arg
    # has no such limit).
    r = subprocess.run(["bash", "-c", f'{SRC}; {call} "$1"', "_", gathered],
                       capture_output=True, text=True, timeout=30, env=dict(os.environ))
    assert r.returncode == 0, f"{call} failed: {r.stderr}"
    return r.stdout.strip()


class SnippetShape(unittest.TestCase):
    def test_snippet_never_cats_the_whole_log(self):
        s = _snippet()
        self.assertNotIn('cat "$f"', s, "the gather must not ship the whole OBS log (1.2 GB live)")
        # It must filter to the union of the five parsers' marker families...
        for anchor in ("genlock:", "projector-vsync:", "video settings reset:", "fps:"):
            self.assertIn(anchor, s, f"the gather must grep the {anchor!r} marker family")
        self.assertIn("grep -aE", s, "the gather must byte-literal grep (-a) the union (-E)")
        # ...and cap its output (head and/or tail).
        self.assertTrue("head -n" in s or "tail -n" in s, "the gather must cap its output")


class BigLogBoundedAndParity(unittest.TestCase):
    def test_middle_markers_survive_and_all_five_facets_parse(self):
        with tempfile.TemporaryDirectory() as home:
            lines = []
            # 150k bulk BEFORE the marker block -> the decisive lines sit in the MIDDLE, so a naive
            # head+tail of the RAW file would miss them; the union grep must not.
            for i in range(150_000):
                lines.append(NOISE_FAMILIES[i % 3].format(s=i % 60, i=i))
            lines += [ADAPTER_FPS, RESET, OUT_FPS, GENLOCK_CAP, GENLOCK_LAT, GENLOCK_RT]
            for i in range(150_000):
                lines.append(NOISE_FAMILIES[i % 3].format(s=i % 60, i=i))
            lines.append(PROJECTOR)  # projector-vsync appears LATE
            for i in range(1_000):
                lines.append(NOISE_FAMILIES[i % 3].format(s=i % 60, i=i))
            _write_log(home, lines)

            r = _bash(_snippet(), home=home)
            self.assertEqual(r.returncode, 0, r.stderr)
            out = r.stdout
            got = out.splitlines()
            # bounded: the ~301k-line log collapses to only the marker lines (a handful), never a GB.
            self.assertLessEqual(len(got), 4000, f"gather shipped {len(got)} lines — unbounded")
            self.assertNotIn("genlock-fifo audit", out, "bulk families must be filtered out")
            self.assertNotIn("program-render-audit", out, "bulk families must be filtered out")
            # facet parity: every parser reads its decisive MIDDLE/LATE line off the bounded text.
            self.assertEqual(_parser(out, "fps_from_log"), "30",
                             "fps_from_log must pick the OUTPUT fps (30), not the adapter decoy (60)")
            self.assertEqual(_parser(out, "genlock_capability_from_log"), "1")
            self.assertEqual(_parser(out, "genlock_latency_ms_from_log"), "3")
            self.assertEqual(_parser(out, "genlock_rt_pin_from_log"), "ok")
            self.assertTrue(_parser(out, "projector_vsync_verdict").startswith("projector_vsync|OK|"))


class HeadAndTailBothKept(unittest.TestCase):
    def test_startup_fps_and_late_projector_both_survive_past_the_cap(self):
        # A pathological filtered stream > 2*cap: the startup fps block, then >2000 `genlock:` lines,
        # then the LATE projector-vsync. A tail-only bound (Approach 1) would drop the startup fps
        # block; the chosen head+tail keeps BOTH ends. This is the per-facet proof for the design.
        with tempfile.TemporaryDirectory() as home:
            lines = [ADAPTER_FPS, RESET, OUT_FPS]  # startup fps block first
            for i in range(2500):
                lines.append(f"07:42:{i % 60:02d}.000: genlock: latency = 3 ms (hot-apply #{i})")
            lines.append(PROJECTOR)  # projector-vsync last
            _write_log(home, lines)

            r = _bash(_snippet(), home=home)
            self.assertEqual(r.returncode, 0, r.stderr)
            out = r.stdout
            self.assertIn(OUT_FPS.strip(), out, "the STARTUP fps block must survive the cap (head)")
            self.assertIn(PROJECTOR, out, "the LATE projector-vsync must survive the cap (tail)")
            self.assertEqual(_parser(out, "fps_from_log"), "30")
            self.assertTrue(_parser(out, "projector_vsync_verdict").startswith("projector_vsync|OK|"))


if __name__ == "__main__":
    unittest.main()
