"""issue 1404 rig race (14.10.2026): scripts/rig-busy-gate.sh admitted the full-path E2E on ONE idle
read that landed inside restreamer's deliberate ~10 s OBS republish gap (its YouTube disconnect
test, gaps up to ~45 s), and it held the shared rig lease the whole time restreamer was streaming,
so restreamer's own restart found the lease held and failed.

The gate must:
- admit a run only after strih + stream have read idle CONTINUOUSLY for the idle hold (60 s, read
  every 5 s = 13 reads), never on a single idle read;
- not hold the rig lease while a box is streaming (a foreign broadcast in progress);
- HOLD the lease through the idle hold (issue 1432, ROZHODNUTÉ 6182938785): every restreamer
  stream-lan job now takes the lease itself for its whole job (restreamer PR 400), so a republish gap
  happens under restreamer's lease; releasing during the hold let restreamer's NEXT job take the rig
  and starved the release E2E (run 38667117544).

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
STRAY = {
    "busy": True,
    "reasons": ["strih is recording (GetRecordStatus.outputActive=true)"],
    "diagnostics": [{"host": "strih", "streaming": False, "recording": True}],
}
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
steal = os.environ.get("FAKE_STEAL_AT")
if steal and n == int(steal) and os.path.isdir(lease_dir):
    # a foreign holder takes over the lockdir mid-hold, declared far past the gate's wait budget
    with open(os.path.join(lease_dir, "holder.json"), "w") as f:
        json.dump({"repo": "zbynekdrlik/restreamer", "run_id": "999", "run_url": "u", "job": "j",
                   "acquired_at": "2026-01-01T00:00:00Z", "expected_release_at": "2099-01-01T00:00:00Z"}, f)
    open(os.path.join(lease_dir, "heartbeat"), "a").close()
    os.utime(os.path.join(lease_dir, "heartbeat"))
with open(log_path, "a") as f:
    f.write("%d %s\n" % (int(os.path.exists(os.path.join(lease_dir, "holder.json"))), "busy" if answer["busy"] else "idle"))
print(json.dumps(answer))
'''


def run_gate(seq, iterations=3, extra_env=None):
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
    env.update(extra_env or {})
    r = subprocess.run(["bash", GATE], env=env, capture_output=True, text=True, timeout=120)
    calls = []
    if os.path.exists(log_path):
        calls = [line.split() for line in open(log_path).read().splitlines()]
    held_at_exit = os.path.exists(os.path.join(lease_dir, "holder.json"))
    r.lease_dir = lease_dir
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
        # issue 1432: the read that breaks the hold runs under the lease taken for it; every read
        # after it sees a box streaming, so the lease is released and stays released
        self.assertEqual(calls[2], ["1", "busy"], calls)
        self.assertTrue(calls[3:], calls)
        self.assertTrue(all(c[0] == "0" for c in calls[3:]), calls)

    def test_a_busy_read_inside_the_hold_ends_the_hold(self):
        r, calls, _held = run_gate([IDLE, STREAMING, IDLE], iterations=1)
        out = r.stdout + r.stderr
        self.assertEqual(r.returncode, 42, out)
        self.assertNotIn("OUTCOME=RIG_FREE", out)

    def test_a_stray_recording_wait_keeps_the_lease_flag_in_step(self):
        # streaming (lease released), then a stray recording (busy, not streaming: the lease is
        # taken back), then idle: the hold must run under that lease, not wait on our own lease
        r, calls, held = run_gate([STREAMING, STRAY, IDLE], iterations=4)
        out = r.stdout + r.stderr
        self.assertEqual(r.returncode, 0, out)
        self.assertIn("OUTCOME=RIG_FREE", out)
        self.assertTrue(held)

    def test_a_lease_taken_over_during_the_hold_never_admits(self):
        # a foreign holder replaces ours on the 5th read (inside the hold); the gate must not admit
        # on a lease that is no longer ours
        r, calls, _held = run_gate([IDLE], iterations=2, extra_env={"FAKE_STEAL_AT": "4"})
        out = r.stdout + r.stderr
        # the gate gives the lease up and waits on the foreign holder, which is declared far past
        # its wait budget: fail fast (exit 44), never admit, never release the foreign lease
        self.assertEqual(r.returncode, 44, out)
        self.assertIn("OUTCOME=RIG_LEASE_HELD", out)
        self.assertNotIn("OUTCOME=RIG_FREE", out)
        self.assertIn("taken over during the idle hold", out)
        with open(os.path.join(r.lease_dir, "holder.json")) as f:
            self.assertEqual(json.load(f)["run_id"], "999", "the foreign lease is never released")

    def test_a_continuous_idle_hold_admits_and_keeps_the_lease(self):
        r, calls, held = run_gate([STREAMING, IDLE], iterations=3)
        out = r.stdout + r.stderr
        self.assertEqual(r.returncode, 0, out)
        self.assertIn("OUTCOME=RIG_FREE", out)
        self.assertTrue(held, "an admitted run keeps the lease for the recording step")
        idle_reads = sum(1 for c in calls if c[1] == "idle")
        # 13 hold reads (60 s / 5 s + 1) plus the final read under the lease
        self.assertGreaterEqual(idle_reads, 14, calls)

    def test_the_lease_is_not_held_while_a_box_streams_but_is_held_through_the_idle_hold(self):
        r, calls, held = run_gate([STREAMING, STREAMING, IDLE], iterations=4)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        # the very first read may run under the initial lease (fail-fast acquire, issue 830);
        # the later streaming read sees it released (a foreign broadcast in progress)
        self.assertEqual(calls[1], ["0", "busy"], calls)
        # the poll read that finds the rig idle still runs with the lease released (it decides to
        # take it back); issue 1432: from there on the lease is held -- the whole idle hold and the
        # final admission read run under it, so no other holder can take the rig in between
        self.assertEqual(calls[2], ["0", "idle"], calls)
        hold = calls[3:]
        self.assertGreaterEqual(len(hold), 13, calls)
        self.assertTrue(all(c == ["1", "idle"] for c in hold), calls)
        self.assertTrue(held)

    def test_an_idle_first_read_keeps_the_initial_lease_through_the_hold(self):
        r, calls, held = run_gate([IDLE], iterations=2)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertTrue(all(c[0] == "1" for c in calls), calls)
        self.assertNotIn("releasing", r.stdout)
        self.assertTrue(held)


if __name__ == "__main__":
    unittest.main()
