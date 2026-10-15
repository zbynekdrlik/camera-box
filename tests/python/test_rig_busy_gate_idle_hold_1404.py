"""issue 1404 rig race (14.10.2026): scripts/rig-busy-gate.sh admitted the full-path E2E on ONE idle
read that landed inside restreamer's deliberate ~10 s OBS republish gap (its YouTube disconnect
test, gaps up to ~45 s), and it held the shared rig lease the whole time restreamer was streaming,
so restreamer's own restart found the lease held and failed.

The gate must:
- admit a run only after strih + stream have read idle CONTINUOUSLY for the idle hold (60 s, read
  every 5 s = 13 reads), never on a single idle read;
- not hold the rig lease while a box is streaming (a foreign broadcast in progress), and not during
  the idle hold either -- the lease is taken only after the hold, with one final idle read under it.

The fake obs_phase2.py serves a scripted sequence of rig-busy-check answers (the last one repeats)
and records, per call, whether the lease holder file existed at that moment.
"""

import json
import os
import subprocess
import tempfile
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
GATE = os.path.join(REPO, "scripts", "rig-busy-gate.sh")

IDLE = {"busy": False, "reasons": [], "diagnostics": []}
STREAMING = {
    "busy": True,
    "reasons": ["stream is streaming (GetStreamStatus.outputActive=true)"],
    "diagnostics": [{"host": "stream", "streaming": True, "recording": False}],
}

FAKE = r'''import json, os, sys
seq_path, log_path, lease_dir = os.environ["FAKE_SEQ"], os.environ["FAKE_LOG"], os.environ["RIG_LEASE_DIR"]
if sys.argv[1:2] != ["rig-busy-check"]:
    sys.exit(0)
seq = json.load(open(seq_path))
n = 0
if os.path.exists(log_path):
    n = sum(1 for _ in open(log_path))
answer = seq[min(n, len(seq) - 1)]
with open(log_path, "a") as f:
    f.write("%d %s\n" % (int(os.path.exists(os.path.join(lease_dir, "holder.json"))), "busy" if answer["busy"] else "idle"))
print(json.dumps(answer))
'''


def run_gate(seq, iterations=3):
    d = tempfile.mkdtemp(prefix="rbg1404-")
    fake = os.path.join(d, "obs_phase2.py")
    with open(fake, "w") as f:
        f.write(FAKE)
    seq_path = os.path.join(d, "seq.json")
    with open(seq_path, "w") as f:
        json.dump(seq, f)
    log_path = os.path.join(d, "calls.log")
    lease_dir = os.path.join(d, "lease")
    env = dict(os.environ)
    for k in ("GITHUB_STEP_SUMMARY", "GITHUB_REPOSITORY", "GITHUB_RUN_ID", "GITHUB_JOB",
              "RIG_BUSY_GATE_IDLE_HOLD_SECS", "RIG_BUSY_GATE_IDLE_POLL_SECS"):
        env.pop(k, None)
    env.update({
        "OBS_PHASE2_PY": fake,
        "FAKE_SEQ": seq_path,
        "FAKE_LOG": log_path,
        "RIG_LEASE_DIR": lease_dir,
        "RIG_LEASE_REPO": "zbynekdrlik/camera-box",
        "RIG_LEASE_RUN_ID": "run-1404",
        "RIG_LEASE_RUN_URL": "https://example/run/1404",
        "RIG_LEASE_JOB": "full-path",
        "RIG_BUSY_GATE_ITERATIONS": str(iterations),
        "RIG_BUSY_GATE_SLEEP_SECS": "0",
    })
    r = subprocess.run(["bash", GATE], env=env, capture_output=True, text=True, timeout=120)
    calls = []
    if os.path.exists(log_path):
        calls = [line.split() for line in open(log_path).read().splitlines()]
    held_at_exit = os.path.exists(os.path.join(lease_dir, "holder.json"))
    return r, calls, held_at_exit


class IdleHold1404(unittest.TestCase):
    def test_one_idle_read_inside_a_republish_gap_never_admits(self):
        # streaming, one idle read (the gap), streaming again for the rest of the budget
        r, calls, held = run_gate([STREAMING, IDLE, STREAMING], iterations=3)
        out = r.stdout + r.stderr
        self.assertEqual(r.returncode, 42, out)
        self.assertIn("OUTCOME=RIG_BUSY", out)
        self.assertNotIn("OUTCOME=RIG_FREE", out)
        self.assertFalse(held, "a refused gate leaves no lease behind")

    def test_a_continuous_idle_hold_admits_and_keeps_the_lease(self):
        r, calls, held = run_gate([STREAMING, IDLE], iterations=3)
        out = r.stdout + r.stderr
        self.assertEqual(r.returncode, 0, out)
        self.assertIn("OUTCOME=RIG_FREE", out)
        self.assertTrue(held, "an admitted run keeps the lease for the recording step")
        idle_reads = sum(1 for c in calls if c[1] == "idle")
        # 13 hold reads (60 s / 5 s + 1) plus the final read under the lease
        self.assertGreaterEqual(idle_reads, 14, calls)

    def test_the_lease_is_not_held_while_a_box_streams_nor_during_the_idle_hold(self):
        r, calls, _ = run_gate([STREAMING, STREAMING, IDLE], iterations=4)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        # the very first read may run under the initial lease (fail-fast acquire, issue 830);
        # every later streaming read and every idle-hold read must see the lease free,
        # and only the final admission read runs under it
        later = calls[1:-1]
        self.assertTrue(later, calls)
        self.assertTrue(all(c[0] == "0" for c in later), calls)
        self.assertEqual(calls[-1], ["1", "idle"], calls)


if __name__ == "__main__":
    unittest.main()
