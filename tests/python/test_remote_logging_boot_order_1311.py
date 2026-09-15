"""Tier-0 boot-order tests for the #1311 off-box remote-logging units (found live on cam2, 15.9.2026).

Both units FAIL at every clean cambox boot and stay dead until a hand restart, because a cambox
MASKS systemd-networkd-wait-online (setup-device.sh STEP 11 / create-usb-linux.sh -- the #547
boot-stall fix), so network-online.target is reached INSTANTLY, before systemd-networkd has applied
the static IP/route:

  * cambox-netconsole.service is a oneshot whose setup script checks the egress route to dev1 ONCE
    (`ip -o route get`) and exit 1s on the FIRST miss -- so the arm dies before the route exists.
  * systemd-journal-upload keeps the stock Restart=on-failure with the default StartLimit, so 5
    instant "connection refused" restarts (dev1 not yet reachable) exhaust the limit and the unit
    stays `failed` for the whole boot.

The fix: the setup script waits for the egress route in a bounded retry loop; the unit orders after
systemd-networkd.service; the journal-upload drop-in sets StartLimitIntervalSec=0 + Restart=always +
RestartSec so it keeps retrying (the uploader is idempotent -- the /run cursor resumes) until dev1
answers.
"""

import os
import stat
import subprocess
import tempfile
import unittest

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "remote-logging.sh")


def _sourced(func: str, env_extra=None) -> str:
    env = dict(os.environ)
    if env_extra:
        env.update(env_extra)
    r = subprocess.run(
        ["bash", "-c", f'set -u; . "{LIB}"; {func}'],
        capture_output=True,
        text=True,
        timeout=30,
        cwd=REPO,
        env=env,
    )
    assert r.returncode == 0, r.stderr
    return r.stdout


# Fake `ip`: `route get` returns EMPTY for the first FAIL_UNTIL calls (route not applied yet), then
# the real dev/src line; `neigh show` always resolves the MAC. Reproduces the late-static-route boot.
FAKE_IP = r"""#!/usr/bin/env bash
CNT="__COUNTER__"
FAIL_UNTIL=__FAIL_UNTIL__
args="$*"
case "$args" in
  *"route get"*)
    n=$(( $(cat "$CNT" 2>/dev/null || echo 0) + 1 ))
    echo "$n" > "$CNT"
    if [ "$n" -le "$FAIL_UNTIL" ]; then exit 0; fi
    echo "10.77.9.200 dev eth0 src 10.77.9.99 uid 0 cache"
    ;;
  *"neigh show"*)
    echo "10.77.9.200 dev eth0 lladdr aa:bb:cc:dd:ee:ff REACHABLE"
    ;;
  *) : ;;
esac
"""

STUB = "#!/usr/bin/env bash\nexit 0\n"


def _write_exec(path: str, body: str) -> None:
    with open(path, "w") as fh:
        fh.write(body)
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)


class NetconsoleWaitsForLateRoute(unittest.TestCase):
    def test_setup_script_retries_the_egress_route_and_arms_after_a_late_route(self):
        with tempfile.TemporaryDirectory() as tmp:
            bindir = os.path.join(tmp, "bin")
            cfg = os.path.join(tmp, "cfg")
            os.makedirs(bindir)
            counter = os.path.join(tmp, "route_calls")
            # Generate the setup script with fast (sleep 0) retries baked in.
            script = _sourced(
                "remote_log_netconsole_setup_script_content",
                {
                    "REMOTE_LOG_NC_CONFIGFS": cfg,
                    "REMOTE_LOG_NC_ROUTE_RETRIES": "30",
                    "REMOTE_LOG_NC_ROUTE_RETRY_SLEEP_S": "0",
                    "REMOTE_LOG_NC_MAC_RETRIES": "5",
                    "REMOTE_LOG_NC_MAC_RETRY_SLEEP_S": "0",
                },
            )
            sp = os.path.join(tmp, "setup.sh")
            with open(sp, "w") as fh:
                fh.write(script)
            fake_ip = FAKE_IP.replace("__COUNTER__", counter).replace("__FAIL_UNTIL__", "3")
            _write_exec(os.path.join(bindir, "ip"), fake_ip)
            for name in ("ping", "modprobe", "mount", "mountpoint"):
                _write_exec(os.path.join(bindir, name), STUB)
            env = dict(os.environ)
            env["PATH"] = bindir + os.pathsep + env["PATH"]
            r = subprocess.run(["bash", sp], capture_output=True, text=True, timeout=60, env=env)
            self.assertEqual(r.returncode, 0, f"stdout={r.stdout}\nstderr={r.stderr}")
            self.assertTrue(
                os.path.exists(os.path.join(cfg, "dev_name")),
                f"dev_name not written -- the setup script gave up instead of retrying; stderr={r.stderr}",
            )
            self.assertEqual(open(os.path.join(cfg, "dev_name")).read().strip(), "eth0")
            self.assertEqual(
                open(os.path.join(cfg, "remote_mac")).read().strip(), "aa:bb:cc:dd:ee:ff"
            )
            # Proof the retry loop actually engaged (the route was polled past the initial misses).
            self.assertGreaterEqual(
                int(open(counter).read().strip()),
                4,
                "the egress route should have been polled past the initial misses (retry loop)",
            )


class NetconsoleUnitOrdersAfterNetworkd(unittest.TestCase):
    def test_unit_orders_after_systemd_networkd(self):
        u = _sourced("remote_log_netconsole_service_unit_content")
        self.assertIn("After=systemd-networkd.service", u, u)
        self.assertIn("Wants=systemd-networkd.service", u, u)
        # The pre-existing network-online ordering stays (harness_remote_logging_1311.rs pins it).
        self.assertIn("After=network-online.target", u, u)
        self.assertIn("Wants=network-online.target", u, u)


class JournalUploadRetriesPastStartLimit(unittest.TestCase):
    def test_dropin_disables_startlimit_and_restarts_always(self):
        d = _sourced("remote_log_journal_upload_dropin_content")
        self.assertIn("StartLimitIntervalSec=0", d, d)
        self.assertIn("Restart=always", d, d)
        self.assertRegex(d, r"(?m)^RestartSec=")


if __name__ == "__main__":
    unittest.main()
