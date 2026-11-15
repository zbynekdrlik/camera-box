"""issue 1426: the fleet version-parity pass (OBS genlock build across both venues + dantesync,
with the dantesync catch-up for a box that comes back after a roll).

Pure decision tests on scripts/fleet_build_parity_decision.py, then the real
scripts/fleet-parity-alert-watchdog.sh driven through its seams (no network: every probe, the two
dantesync scripts, the rig-busy read and the notify are stubs; the obs-fleet resolve / port-probe /
identity seams decide who is home, so nothing reaches the live rig from dev1).
PP not powered is NORMAL (owner 14.10.2026): an away box is never a page and never blocks a roll.
"""
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(REPO, "scripts"))
import fleet_build_parity_decision as d  # noqa: E402

WATCHDOG = os.path.join(REPO, "scripts", "fleet-parity-alert-watchdog.sh")
A = "fd25d1d30f3382cc"  # a git SHA prefix (synthetic, kept short)
B = "1085770d6da9fc7d"


class Decision(unittest.TestCase):
    def test_one_build_everywhere_is_ok(self):
        r = d.classify([{"box": "strih-snv", "state": "present", "sha": A},
                        {"box": "stream-snv", "state": "present", "sha": A},
                        {"box": "strih-pp", "state": "away"}])
        self.assertEqual(r["verdict"], "OK")
        self.assertEqual(r["away"], "strih-pp")

    def test_an_outlier_is_drift_named_against_the_majority(self):
        r = d.classify([{"box": "strih-snv", "state": "present", "sha": A},
                        {"box": "stream-snv", "state": "present", "sha": A},
                        {"box": "resolume-snv", "state": "present", "sha": B}])
        self.assertEqual(r["verdict"], "DRIFT")
        self.assertEqual(r["modal"], A)
        self.assertEqual(r["outliers"], "resolume-snv:" + B[:12])

    def test_a_blind_home_box_is_incomplete_never_ok(self):
        r = d.classify([{"box": "strih-snv", "state": "present", "sha": A},
                        {"box": "resolume-pp", "state": "unreadable"}])
        self.assertEqual(r["verdict"], "INCOMPLETE")
        self.assertEqual(r["blind"], "resolume-pp(unreadable)")

    def test_a_whole_venue_off_is_not_a_fault(self):
        r = d.classify([{"box": b, "state": "away"} for b in ("strih-pp", "stream-pp", "resolume-pp")]
                       + [{"box": "strih-snv", "state": "present", "sha": A}])
        self.assertEqual(r["verdict"], "OK")

    def test_nothing_present_is_unknown(self):
        self.assertEqual(d.classify([{"box": "strih-pp", "state": "away"}])["verdict"], "UNKNOWN")

    def test_a_tie_picks_one_modal_and_still_drifts(self):
        r = d.classify([{"box": "x", "state": "present", "sha": A},
                        {"box": "y", "state": "present", "sha": B}])
        self.assertEqual(r["verdict"], "DRIFT")
        self.assertEqual(r["tie"], "1")


def _exe(path, body):
    with open(path, "w") as f:
        f.write(body)
    os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)


GATE_OK = "  cam2           1.17.0       OK\n  dev1           1.17.0       OK\n"
GATE_DRIFT = GATE_OK + "  cam4           1.16.0       DRIFT    (expected 1.17.0)\n"


class Watchdog(unittest.TestCase):
    def run_pass(self, shas, home, gate_out, gate_rc, busy="False", dry=True, state=None, lease=False,
                 ports=None):
        d_ = state or tempfile.mkdtemp(prefix="fp1426-")
        # ports: host -> the ports that answer (4455 = OBS-WS, 8899 = BundleStateServer); a host not
        # listed answers nothing, so a traveling box is away unless `home` names it.
        ports = ports or {}
        resolve = os.path.join(d_, "resolve")
        _exe(resolve, "#!/usr/bin/env bash\necho 192.0.2.10\n")
        probe = os.path.join(d_, "probe")
        _exe(probe, "#!/usr/bin/env bash\ncase \"$1:$2\" in\n" + "".join(
            "  %s:%d) exit 0 ;;\n" % (h, p) for h, ps in ports.items() for p in ps)
            + "  *) exit 1 ;;\nesac\n")
        ident = os.path.join(d_, "ident")
        _exe(ident, "#!/usr/bin/env bash\necho \"$1\"\n")
        fetch = os.path.join(d_, "fetch")
        # shas: host -> sha ("" = :8899 without the facet, None = :8899 down)
        _exe(fetch, "#!/usr/bin/env bash\ncase \"$1\" in\n" + "".join(
            "  %s) %s ;;\n" % (h, "exit 1" if s is None else "printf '%%s' '%s'" % json.dumps(
                {"genlock_build_sha": s} if s else {"profile": "x"})) for h, s in shas.items())
            + "  *) exit 1 ;;\nesac\n")
        gate = os.path.join(d_, "gate")
        _exe(gate, "#!/usr/bin/env bash\nprintf '%%s' '%s'\nexit %d\n" % (gate_out, gate_rc))
        upgrade = os.path.join(d_, "upgrade")
        _exe(upgrade, "#!/usr/bin/env bash\necho UPGRADE-RAN >> '%s/upgrade.log'\nexit 0\n" % d_)
        busyc = os.path.join(d_, "busy")
        _exe(busyc, "#!/usr/bin/env bash\necho \"$1 $2\" >> '%s/busy.log'\nprintf '{\"busy\": %s}'\n"
             % (d_, "true" if busy == "True" else "false"))
        notify = os.path.join(d_, "notify.py")
        _exe(notify, "import sys\nopen('%s/notify.log','a').write(' '.join(sys.argv[1:])+'\\n')\n" % d_)
        lease_dir = os.path.join(d_, "lease")
        if lease:
            os.makedirs(lease_dir)
            open(os.path.join(lease_dir, "holder.json"), "w").write("{}")
            open(os.path.join(lease_dir, "heartbeat"), "w").write("")
        env = dict(os.environ)
        env.update({
            "FLEET_PARITY_FETCH_CMD": fetch, "FLEET_PARITY_DSYNC_GATE_CMD": gate,
            "FLEET_PARITY_DSYNC_UPGRADE_CMD": upgrade, "FLEET_PARITY_BUSY_CMD": busyc,
            "AIRULESET_NOTIFY": notify, "FLEET_PARITY_STATE_FILE": os.path.join(d_, "state"),
            "RIG_LEASE_DIR": lease_dir, "OBS_FLEET_RESOLVE_CMD": resolve,
            "OBS_FLEET_STATUS_PROBE_CMD": probe, "OBS_FLEET_IDENTITY_CMD": ident,
        })
        env.pop("OBS_FLEET_HOME", None)
        if home:
            env["OBS_FLEET_HOME"] = " ".join(home)
        args = ["bash", WATCHDOG] + (["--dry-run"] if dry else [])
        r = subprocess.run(args, env=env, capture_output=True, text=True, timeout=60)
        read = lambda n: open(os.path.join(d_, n)).read() if os.path.exists(os.path.join(d_, n)) else ""
        return r, read, d_

    SNV = {"10.77.9.202": A, "10.77.9.204": A}
    SNV_HOME = []   # strih-snv / stream-snv are `always`; the PP boxes answer nothing = away

    def test_build_parity_covers_the_production_boxes_only(self):
        # owner 16.10.2026: the cg OBS on the resolume PCs has minimal priority, no tokens on it, so
        # neither resolume box is compared or paged; every other OBS box of both venues is.
        r = subprocess.run(["bash", "-c", '. scripts/lib/obs-fleet.sh; obs_fleet_boxes build-parity'],
                           cwd=REPO, capture_output=True, text=True, timeout=30)
        names = [pair.split("|")[0] for pair in r.stdout.split()]
        self.assertEqual(names, ["strih-snv", "stream-snv", "strih-pp", "stream-pp"], r.stderr)

    def test_an_idle_box_with_obs_stopped_is_still_compared(self):
        # stream-pp 26.10.2026: OBS not running (:4455 refused) while :8899 serves the installed build.
        shas = dict(self.SNV, **{"stream-pp.lan": B})
        r, read, _ = self.run_pass(shas, self.SNV_HOME, GATE_OK, 0, ports={"stream-pp.lan": [8899]})
        self.assertIn("verdict=DRIFT", r.stderr)
        self.assertIn("stream-pp:" + B[:12], r.stderr)
        self.assertIn("away=strih-pp", r.stderr)
        self.assertNotIn("away=strih-pp,stream-pp", r.stderr)

    def test_a_box_that_answers_nothing_stays_away(self):
        r, read, _ = self.run_pass(dict(self.SNV, **{"stream-pp.lan": B}), self.SNV_HOME, GATE_OK, 0)
        self.assertIn("verdict=OK", r.stderr)
        self.assertIn("away=strih-pp,stream-pp", r.stderr)

    def test_pp_off_and_one_build_is_quiet(self):
        r, read, _ = self.run_pass(self.SNV, self.SNV_HOME, GATE_OK, 0, dry=False)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("verdict=OK", r.stderr)
        self.assertIn("away=strih-pp,stream-pp", r.stderr)
        self.assertNotIn("resolume", r.stderr)
        self.assertEqual(read("notify.log"), "")

    def test_a_returning_box_on_an_old_build_pages_after_the_confirm(self):
        shas = dict(self.SNV, **{"stream-pp.lan": B})
        up = {"stream-pp.lan": [4455, 8899]}
        r, read, d_ = self.run_pass(shas, self.SNV_HOME, GATE_OK, 0, dry=False, ports=up)
        self.assertIn("verdict=DRIFT", r.stderr)
        self.assertEqual(read("notify.log"), "", "one pass is not confirmed")
        r, read, _ = self.run_pass(shas, self.SNV_HOME, GATE_OK, 0, dry=False, state=d_, ports=up)
        log = read("notify.log")
        self.assertIn("--dedup-key fleet-parity", log)
        self.assertIn("stream-pp:" + B[:12], log)

    def test_a_dantesync_drift_is_caught_up_on_an_idle_rig(self):
        r, read, _ = self.run_pass(self.SNV, self.SNV_HOME, GATE_DRIFT, 20, dry=False)
        self.assertIn("UPGRADE-RAN", read("upgrade.log"), r.stderr)
        self.assertIn("dorovnané", r.stderr)
        self.assertEqual(read("notify.log"), "")

    def test_no_catch_up_while_a_box_streams(self):
        r, read, _ = self.run_pass(self.SNV, self.SNV_HOME, GATE_DRIFT, 20, busy="True", dry=False)
        self.assertEqual(read("upgrade.log"), "", r.stderr)
        self.assertIn("odložené", r.stderr)

    def test_no_catch_up_while_the_rig_lease_is_held(self):
        r, read, _ = self.run_pass(self.SNV, self.SNV_HOME, GATE_DRIFT, 20, dry=False, lease=True)
        self.assertEqual(read("upgrade.log"), "", r.stderr)
        self.assertIn("rig lease", r.stderr)

    def test_an_away_venue_is_never_read_for_busy(self):
        r, read, _ = self.run_pass(self.SNV, self.SNV_HOME, GATE_DRIFT, 20, dry=False)
        self.assertNotIn("strih-pp.lan", read("busy.log"))

    def test_dry_run_never_rolls_or_pages(self):
        shas = dict(self.SNV, **{"stream-pp.lan": B})
        up = {"stream-pp.lan": [4455, 8899]}
        r, read, d_ = self.run_pass(shas, self.SNV_HOME, GATE_DRIFT, 20, ports=up)
        self.run_pass(shas, self.SNV_HOME, GATE_DRIFT, 20, state=d_, ports=up)
        self.assertEqual(read("upgrade.log") + read("notify.log"), "")


if __name__ == "__main__":
    unittest.main()
