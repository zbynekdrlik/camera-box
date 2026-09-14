"""Tier-0 tests for scripts/lib/rig-grandmaster.sh (#1307): the ONE source of truth for the rig's
PTP grandmaster address. Owner directive 2026-09-13: the grandmaster is addressed by DNS name
`video-clock.lan`, never a literal IP; unresolvable must fail LOUD (early-gate-pin doctrine),
never fall back to a stale literal.

Each test sources the lib in a fresh bash and drives `rig_grandmaster_ip` through the
RIG_GRANDMASTER_GETENT seam (a fake `getent` on disk) so no real DNS is consulted.
"""

import os
import stat
import subprocess
import tempfile
import unittest

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "rig-grandmaster.sh")


def _fake_getent(dirpath: str, mapping: dict) -> str:
    """Write a fake `getent ahostsv4 HOST` that answers from `mapping` (host -> ip) and fails
    (rc 2, no output) for anything else -- the real getent's shape for an unknown host."""
    path = os.path.join(dirpath, "fake-getent")
    lines = ["#!/usr/bin/env bash", 'db="$1"; host="$2"', '[ "$db" = ahostsv4 ] || exit 2']
    for host, ip in mapping.items():
        lines.append(f'if [ "$host" = "{host}" ]; then printf "%s STREAM {host}\\n%s DGRAM\\n" "{ip}" "{ip}"; exit 0; fi')
    lines.append("exit 2")
    with open(path, "w") as fh:
        fh.write("\n".join(lines) + "\n")
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC)
    return path


def _run(env_extra: dict, fake: str) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith("RIG_GRANDMASTER_")}
    env.update({"RIG_GRANDMASTER_GETENT": fake})
    env.update(env_extra)
    script = f'set -u; . "{LIB}"; if ip="$(rig_grandmaster_ip)"; then echo "IP=$ip"; else echo "RC=$?"; fi'
    return subprocess.run(["bash", "-c", script], env=env, capture_output=True, text=True, timeout=20)


class RigGrandmasterResolver(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.fake = _fake_getent(self.tmp.name, {"video-clock.lan": "10.77.9.230", "other.lan": "10.77.9.99"})

    def tearDown(self):
        self.tmp.cleanup()

    def test_lib_is_source_only_and_never_sets_errexit(self):
        text = open(LIB).read()
        self.assertIn("airuleset:script-ok source-only lib", text)
        self.assertNotIn("\nset -e", text, "a sourced lib must never leak set -e into its caller")

    def test_default_host_is_video_clock_lan_resolved_via_getent(self):
        r = _run({}, self.fake)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "IP=10.77.9.230")
        self.assertEqual(r.stderr.strip(), "", "a clean resolve must print nothing on stderr")

    def test_explicit_ip_override_wins_without_touching_dns(self):
        dead = _fake_getent(self.tmp.name + "/", {})  # would fail for every host
        r = _run({"RIG_GRANDMASTER_IP": "10.77.9.184"}, dead)
        self.assertEqual(r.stdout.strip(), "IP=10.77.9.184")

    def test_host_override_is_honoured(self):
        r = _run({"RIG_GRANDMASTER_HOST": "other.lan"}, self.fake)
        self.assertEqual(r.stdout.strip(), "IP=10.77.9.99")

    def test_unresolvable_fails_loud_never_a_stale_literal(self):
        r = _run({"RIG_GRANDMASTER_HOST": "does-not-exist.lan"}, self.fake)
        self.assertEqual(r.stdout.strip(), "RC=1", "must return rc 1, never print a fallback IP")
        self.assertIn("cannot resolve the PTP grandmaster host 'does-not-exist.lan'", r.stderr)
        self.assertIn("RIG_GRANDMASTER_IP", r.stderr, "the message must name the deliberate override knob")
        self.assertNotIn("10.77.9.184", r.stdout, "no silent fallback to the retired literal")

    def test_gate_failure_message_never_names_the_retired_literal(self):
        """The dantesync-gate FAIL hint must name the RESOLVED grandmaster (or the DNS name), never
        the retired literal -- live 14.9.2026 the gate refused an E2E with 'Bring GM 10.77.9.184 up'
        while every node was locked to 10.77.9.230 (the message lied about which GM to bring up)."""
        text = open(os.path.join(REPO, "scripts", "dantesync-gate.sh")).read()
        self.assertNotIn("Bring GM 10.77.9.184", text, "dantesync-gate.sh still hard-codes the retired GM in its failure hint")
        self.assertIn('Bring GM ${GATE_GRANDMASTER_IP}', text, "the hint must interpolate the resolved grandmaster")

    def test_no_literal_184_default_survives_in_the_consumers(self):
        """The retired literal must not remain as a DEFAULT in any consumer (prose mentions of the
        incident are fine; a `${RIG_GRANDMASTER_IP:-10.77.9.184}` default is not)."""
        for rel in ("scripts/dantesync-gate.sh", "scripts/verify-imag.sh", "scripts/setup-imag.sh", "scripts/setup-device.sh"):
            text = open(os.path.join(REPO, rel)).read()
            self.assertNotIn(":-10.77.9.184}", text, f"{rel} still defaults the grandmaster to the retired literal")
            self.assertIn("rig-grandmaster.sh", text, f"{rel} must source the shared resolver")


if __name__ == "__main__":
    unittest.main()
