"""Tier-0 tests for issue 1342 -- the camboxes publish ONE NDI output and the fleet discovers via the
NDI Discovery Server on dev1.

Two halves, both pure-bash / file-content checks (no cargo, no rig):

1. The unconsumed issue-792 `CAMn (30p)` blend stream is GONE: no `src/publish_30p.rs`, no wiring
   in `src/lib.rs` / `src/main.rs`, setup-device.sh no longer writes `publish-30p.conf` and REMOVES a
   leftover one on re-provision (so a live box converges), verify-device.sh has no `(z)` 30p check.
2. The discovery config: the ONE source of truth `scripts/lib/ndi-discovery.sh` renders
   `ndi-config.v1.json` (`networks.discovery` = dev1's rig IP, `networks.ips` = "") and grades it;
   setup-device / setup-strih write it, verify-device `(an)` / verify-strih item 34 grade it, the
   checked-in laptop config is byte-identical to the lib's rendering, the Windows `.ps1` writes it
   BOM-free, and the dev1 `ndi-discovery-server` user unit ships disabled.
"""

import json
import os
import re
import subprocess
import tempfile
import unittest

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "ndi-discovery.sh")
SETUP_DEVICE = os.path.join(REPO, "scripts", "setup-device.sh")
VERIFY_DEVICE = os.path.join(REPO, "scripts", "verify-device.sh")
SETUP_STRIH = os.path.join(REPO, "scripts", "setup-strih.sh")
VERIFY_STRIH = os.path.join(REPO, "scripts", "verify-strih.sh")
LAPTOP_JSON = os.path.join(REPO, "scripts", "ndi-discovery", "ndi-config.v1.json")
LAPTOP_PS1 = os.path.join(REPO, "scripts", "ndi-discovery-laptop.ps1")
DEV1_UNIT = os.path.join(REPO, "systemd", "ndi-discovery-server.service")
RULE = os.path.join(REPO, ".claude", "rules", "ndi-discovery.md")
CLAUDE_MD = os.path.join(REPO, "CLAUDE.md")

DEV1_RIG_IP = "10.77.9.200"


def _read(path: str) -> str:
    with open(path, encoding="utf-8") as fh:
        return fh.read()


def _bash(script: str, env=None) -> subprocess.CompletedProcess:
    e = dict(os.environ)
    e.pop("NDI_DISCOVERY_SERVERS", None)
    if env:
        e.update(env)
    return subprocess.run(
        ["bash", "-c", script], capture_output=True, text=True, timeout=30, cwd=REPO, env=e
    )


def _lib(body: str, env=None) -> subprocess.CompletedProcess:
    return _bash(f'set -uo pipefail\n. "{LIB}"\n{body}', env=env)


def _live_flow(path: str, guard: str) -> str:
    text = _read(path)
    pos = text.find(guard)
    assert pos >= 0, f"source-guard marker {guard!r} missing from {path}"
    return text[pos:]


def _strip_comments(text: str) -> str:
    return "\n".join(l for l in text.splitlines() if not l.lstrip().startswith("#"))


# --------------------------------------------------------------------------------------------------
# Part 1 -- the 30p stream is removed end to end
# --------------------------------------------------------------------------------------------------


class ThirtyPStreamRemoved(unittest.TestCase):
    def test_publish_30p_module_is_deleted(self):
        self.assertFalse(
            os.path.exists(os.path.join(REPO, "src", "publish_30p.rs")),
            "src/publish_30p.rs must be deleted (0 consumers of CAMn (30p), issue 1342)",
        )

    def test_lib_and_main_carry_no_publish_30p_reference(self):
        for rel in ("src/lib.rs", "src/main.rs", "src/affinity.rs"):
            text = _read(os.path.join(REPO, rel))
            self.assertNotRegex(
                text, r"publish_30p|PUBLISH_30P|publish-30p", f"{rel} still references the 30p publisher"
            )

    def test_setup_device_no_longer_writes_the_30p_dropin(self):
        live = _strip_comments(_live_flow(SETUP_DEVICE, "stop here -- never run the destructive"))
        self.assertNotIn("CAMERA_BOX_PUBLISH_30P", live)
        self.assertNotRegex(live, r"cat\s*>\s*\S*publish-30p\.conf", "STEP 7 must not WRITE publish-30p.conf")

    def test_setup_device_removes_a_leftover_30p_dropin_so_live_boxes_converge(self):
        live = _strip_comments(_live_flow(SETUP_DEVICE, "stop here -- never run the destructive"))
        self.assertRegex(
            live,
            r"rm -f /etc/systemd/system/camera-box\.service\.d/publish-30p\.conf",
            "a re-provision must DELETE a leftover publish-30p.conf (the live boxes still carry it)",
        )
        rm = live.find("publish-30p.conf")
        reload = live.find("systemctl daemon-reload", rm)
        self.assertGreater(reload, rm, "the removal must happen before STEP 7's daemon-reload")

    def test_verify_device_has_no_30p_check_or_parsers(self):
        text = _read(VERIFY_DEVICE)
        for needle in ("publish_30p", "PUBLISH_30P", "publish-30p", "(30p)"):
            self.assertNotIn(needle, text, f"verify-device.sh still carries {needle!r}")
        self.assertNotRegex(text, r"(?m)^# \(z\) ", "the (z) 30p exec block must be removed")


# --------------------------------------------------------------------------------------------------
# Part 2 -- the shared discovery lib (pure functions)
# --------------------------------------------------------------------------------------------------


class DiscoveryLib(unittest.TestCase):
    def test_default_server_is_dev1_rig_ip(self):
        r = _lib('printf "%s" "$NDI_DISCOVERY_SERVERS"')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, DEV1_RIG_IP)

    def test_config_json_is_valid_and_carries_discovery_and_empty_ips(self):
        r = _lib("ndi_discovery_config_json")
        self.assertEqual(r.returncode, 0, r.stderr)
        doc = json.loads(r.stdout)
        self.assertEqual(doc["ndi"]["networks"]["discovery"], DEV1_RIG_IP)
        self.assertEqual(doc["ndi"]["networks"]["ips"], "")
        self.assertTrue(r.stdout.endswith("}\n"), "rendered file ends with a single newline")
        self.assertFalse(r.stdout.startswith(chr(0xFEFF)), "never a BOM")

    def test_config_json_takes_a_redundant_server_list(self):
        r = _lib('ndi_discovery_config_json "10.77.9.200,10.77.9.202"')
        self.assertEqual(json.loads(r.stdout)["ndi"]["networks"]["discovery"], "10.77.9.200,10.77.9.202")

    def test_env_override_changes_the_default(self):
        r = _lib("ndi_discovery_config_json", env={"NDI_DISCOVERY_SERVERS": "10.1.2.3"})
        self.assertEqual(json.loads(r.stdout)["ndi"]["networks"]["discovery"], "10.1.2.3")

    def test_parsers_read_discovery_and_ips(self):
        text = '{"ndi": {"networks": {"ips": "10.77.8.51,10.77.8.52", "discovery": "10.77.9.200"}}}'
        r = _lib(f"T='{text}'; ndi_discovery_config_servers \"$T\"; ndi_discovery_config_ips \"$T\"")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines(), [DEV1_RIG_IP, "10.77.8.51,10.77.8.52"])

    def test_parsers_are_empty_and_exit_zero_on_missing_keys(self):
        r = _lib("ndi_discovery_config_servers ''; echo \"rc=$?\"; ndi_discovery_config_ips '{}'; echo \"rc=$?\"")
        self.assertEqual(r.stdout.splitlines(), ["rc=0", "rc=0"])

    def test_verdict_ok_on_the_rendered_config(self):
        r = _lib('ndi_discovery_config_verdict "$(ndi_discovery_config_json)"')
        self.assertEqual(r.stdout.strip(), "ok", r.stdout + r.stderr)

    def test_verdict_tolerates_spaces_in_the_server_list(self):
        text = '{"ndi":{"networks":{"ips":"","discovery":"10.77.9.200, 10.77.9.202"}}}'
        r = _lib(f"ndi_discovery_config_verdict '{text}' '10.77.9.200,10.77.9.202'")
        self.assertEqual(r.stdout.strip(), "ok", r.stdout)

    def test_verdict_fails_each_drift_facet(self):
        cases = {
            "": "missing",
            '{"ndi":{"networks":{"ips":"","discovery":"10.77.8.9"}}}': "discovery",
            '{"ndi":{"networks":{"ips":"10.77.8.51","discovery":"10.77.9.200"}}}': "ips",
            '{"ndi":{"networks":{"ips":"","discovery":"10.77.9.200"}}': "JSON",
        }
        for text, facet in cases.items():
            r = _lib(f"ndi_discovery_config_verdict '{text}'")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertTrue(r.stdout.startswith("FAIL:"), f"{text!r} must FAIL, got {r.stdout!r}")
            self.assertIn(facet, r.stdout, f"{text!r} verdict must name the {facet} facet: {r.stdout!r}")

    def test_dropin_points_the_service_at_the_system_config_dir(self):
        r = _lib('ndi_discovery_dropin_content; printf "DIR=%s\\n" "$(ndi_discovery_dropin_config_dir "$(ndi_discovery_dropin_content)")"')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertRegex(r.stdout, r"(?m)^\[Service\]$")
        self.assertRegex(r.stdout, r"(?m)^Environment=NDI_CONFIG_DIR=/etc/ndi$")
        self.assertIn("DIR=/etc/ndi", r.stdout)

    def test_dropin_parser_empty_when_absent(self):
        r = _lib("ndi_discovery_dropin_config_dir ''; echo \"rc=$?\"")
        self.assertEqual(r.stdout.strip(), "rc=0")

    def test_write_config_writes_the_rendered_file_idempotently(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = os.path.join(tmp, "a", ".ndi")
            for _ in range(2):
                r = _lib(f'ndi_discovery_write_config "{target}"')
                self.assertEqual(r.returncode, 0, r.stderr)
            path = os.path.join(target, "ndi-config.v1.json")
            want = _lib("ndi_discovery_config_json").stdout
            self.assertEqual(_read(path), want)
            self.assertEqual(oct(os.stat(path).st_mode & 0o777), "0o644")
            self.assertEqual(os.listdir(target), ["ndi-config.v1.json"], "no temp file left behind")

    def test_write_config_fails_loud_on_an_unwritable_dir(self):
        r = _lib('ndi_discovery_write_config "/proc/ndi-discovery-cannot-exist"; echo "rc=$?"')
        self.assertNotIn("rc=0", r.stdout)


# --------------------------------------------------------------------------------------------------
# Part 2 -- provisioning + grading wiring
# --------------------------------------------------------------------------------------------------


class CamboxWiring(unittest.TestCase):
    def test_setup_device_sources_the_lib_and_writes_config_and_dropin_in_step_7(self):
        text = _read(SETUP_DEVICE)
        self.assertRegex(text, r'(?m)^\. "\$HERE/lib/ndi-discovery\.sh"')
        live = _strip_comments(_live_flow(SETUP_DEVICE, "stop here -- never run the destructive"))
        write = live.find('ndi_discovery_write_config "$NDI_DISCOVERY_SYSTEM_DIR"')
        dropin = live.find('ndi_discovery_dropin_content > "$NDI_DISCOVERY_CAMBOX_DROPIN"')
        self.assertGreaterEqual(write, 0, "STEP 7 must write /etc/ndi/ndi-config.v1.json")
        self.assertGreaterEqual(dropin, 0, "STEP 7 must write the camera-box NDI_CONFIG_DIR drop-in")
        reload = live.find("systemctl daemon-reload", max(write, dropin))
        step8 = live.find("[8/${TOTAL_STEPS}]")
        self.assertTrue(0 <= reload < step8, "both writes sit inside STEP 7, before its daemon-reload")

    def test_verify_device_an_check_is_documented_three_times_and_before_q(self):
        text = _read(VERIFY_DEVICE)
        self.assertRegex(text, r'(?m)^\. "\$HERE/lib/ndi-discovery\.sh"')
        self.assertGreaterEqual(len(re.findall(r"\(an\) NDI discovery", text)), 3, "header + usage + exec block")
        an = text.find("# (an) NDI discovery")
        q = text.rfind("# (q) .bak cruft drift")
        self.assertTrue(0 <= an < q, "(an) must sit BEFORE (q), which stays the last check")
        block = text[an:q]
        self.assertIn("ndi_discovery_config_verdict", block)
        self.assertIn("ndi_discovery_dropin_config_dir", block)
        self.assertIn("fail ", block)
        self.assertNotIn('warn "', block)


class StrihWiring(unittest.TestCase):
    def test_setup_strih_writes_the_user_and_system_configs_and_the_intercom_dropin(self):
        text = _read(SETUP_STRIH)
        self.assertRegex(text, r'(?m)^\. "\$\{HERE\}/lib/ndi-discovery\.sh"')
        code = _strip_comments(text)
        self.assertIn('ndi_discovery_write_config "${USER_HOME}/.ndi" "$DESKTOP_USER"', code)
        self.assertIn('ndi_discovery_write_config "$NDI_DISCOVERY_SYSTEM_DIR"', code)
        self.assertIn("ndi_discovery_dropin_content > /etc/systemd/system/intercom-hub.service.d/ndi-discovery.conf", code)

    def test_verify_strih_grades_the_config(self):
        text = _read(VERIFY_STRIH)
        self.assertRegex(text, r'(?m)^\. "\$\{HERE\}/lib/ndi-discovery\.sh"')
        item = text.find("# 34) NDI discovery")
        self.assertGreaterEqual(item, 0)
        # Item 34 sits BEFORE item 32: the pre-existing item-33 test slices "# 33)" to the closing
        # summary, so nothing may follow item 33.
        end = text.find("\n# 32) the shared OBS-box appliance baseline", item)
        self.assertGreater(end, item, "item 34 must sit before item 32")
        self.assertLess(text.find("# 33) NO realtime-priority grant"), text.find('echo ""\nif [ "$FAILS" -eq 0 ]'))
        self.assertGreater(text.find("# 33) NO realtime-priority grant"), end, "item 33 stays the last item")
        block = text[item:end]
        self.assertIn("ndi_discovery_config_verdict", block)
        self.assertIn("bad ", block)


# --------------------------------------------------------------------------------------------------
# Part 2 -- laptop kit, dev1 server unit, rule
# --------------------------------------------------------------------------------------------------


class LaptopKit(unittest.TestCase):
    def test_checked_in_config_is_byte_identical_to_the_lib_rendering(self):
        want = _lib("ndi_discovery_config_json").stdout
        with open(LAPTOP_JSON, "rb") as fh:
            raw = fh.read()
        self.assertFalse(raw.startswith(b"\xef\xbb\xbf"), "no UTF-8 BOM")
        self.assertEqual(raw.decode("utf-8"), want)

    def test_ps1_writes_programdata_config_without_a_bom_and_merges(self):
        text = _read(LAPTOP_PS1)
        self.assertIn("$ErrorActionPreference = 'Stop'", text)
        self.assertIn("ProgramData", text)
        self.assertIn("ndi-config.v1.json", text)
        self.assertIn("UTF8Encoding($false)", text, "BOM-free writer (the dantesync BOM incident)")
        self.assertIn("[System.IO.File]::WriteAllText", text)
        self.assertNotRegex(text, r"(?i)\b(Set-Content|Out-File|Add-Content)\b", "these write a BOM on PS 5.1")
        self.assertIn("ConvertFrom-Json", text, "an existing config is merged, not clobbered")
        self.assertIn("-DryRun", text)
        default = re.search(r"\[string\]\$Server\s*=\s*'([^']+)'", text)
        self.assertIsNotNone(default, "the -Server parameter needs a literal default")
        self.assertEqual(default.group(1), DEV1_RIG_IP, "the .ps1 default must equal the lib constant")


class Dev1ServerUnit(unittest.TestCase):
    def test_unit_runs_the_sdk_server_with_restart_and_hardening(self):
        text = _read(DEV1_UNIT)
        self.assertRegex(text, r"(?m)^ExecStart=%h/\.local/bin/ndi-discovery-server\b")
        self.assertRegex(text, r"(?m)^Restart=always$")
        self.assertRegex(text, r"(?m)^NoNewPrivileges=yes$")
        self.assertIn("VERIFIED-INERT", text, "the dev1 --user hardening caveat must be stated")
        self.assertRegex(text, r"(?m)^WantedBy=default\.target$")

    def test_unit_ships_disabled_install_is_a_supervisor_step(self):
        rule = _read(RULE)
        self.assertIn("systemctl --user enable --now ndi-discovery-server.service", rule)
        self.assertIn("ships DISABLED", rule)


class Rule(unittest.TestCase):
    def test_rule_has_paths_frontmatter_and_router_line(self):
        text = _read(RULE)
        self.assertTrue(text.startswith("---\npaths:\n"), "path-scoped rule")
        front = text.split("---", 2)[1]
        for p in ("scripts/lib/ndi-discovery.sh", "systemd/ndi-discovery-server.service", "scripts/ndi-discovery-laptop.ps1"):
            self.assertIn(p, front)
        self.assertIn(".claude/rules/ndi-discovery.md", _read(CLAUDE_MD))

    def test_rule_states_the_sender_mdns_caveat_and_rollout_order(self):
        text = _read(RULE)
        self.assertIn("Senders, however, will avoid using mDNS", text)
        self.assertIn("receivers first", text.lower())


if __name__ == "__main__":
    unittest.main()
