"""Tier-0 tests for issue 1389: the NDI extra-IP list (`networks.ips`, issue 1342) names only the
obs-fleet `ndi-sender` hosts, NEVER a cambox.

A remote NDI finder with a cambox on its extra-IP list holds a TCP discovery connection into that
cambox's `:5960` listener, and libndi 6.3.2 intermittently aborts the whole camera-box process (an
uncaught `std::system_error` in its own `disc:recv` thread) when that connection closes. The camera
walk stays as the FORBIDDEN set: the generator drops a cambox IP by construction, the verdict FAILs a
config that lists one, the Windows `.ps1` removes them, and `--cambox-apply` rewrites ONLY a
cambox's `/etc/ndi/ndi-config.v1.json` inside its read-only-root window.

The shared helpers and the issue-1342 tests live in tests/python/test_ndi_discovery_1342.py; the
grader-behaviour cases for the cambox facet stay next to their harnesses there.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from test_ndi_discovery_1342 import (  # noqa: E402
    ISSUE_1342_LIST,
    LIB,
    REPO,
    _bash,
    _camera_ips,
    _lib,
    _pinned,
    _read,
)


class CamboxFreeList1389(unittest.TestCase):
    """A remote finder with a cambox on its extra-IP list holds a TCP discovery connection into the
    cambox's :5960 listener; libndi 6.3.2 aborts camera-box when such a connection closes."""

    def test_neither_mode_lists_a_cambox(self):
        stub = 'ndi_discovery_resolve_ipv4() { [ "$1" = resolume.lan ] && printf "10.77.9.201\\n"; return 0; }'
        for mode in ("pinned", "resolve"):
            r = _lib(f"{stub}\nndi_discovery_sender_ips {mode}")
            self.assertEqual(r.returncode, 0, r.stderr)
            got = r.stdout.strip().split(",")
            self.assertFalse(set(got) & set(_camera_ips()), f"{mode}: a cambox IP is on the list: {got}")
        self.assertEqual(got, ["10.77.9.202", "10.77.9.204", "10.77.9.201"])

    def test_a_fleet_host_on_a_cambox_ip_is_dropped_and_named(self):
        fleet = ("strih-lx|10.77.9.61|linux-genlock|always\nstream|10.77.9.204|windows-genlock|always\n"
                 "resolume|resolume.lan|windows-genlock|traveling")
        r = _lib("ndi_discovery_sender_ips pinned", env={"OBS_FLEET": fleet})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "10.77.9.204")
        self.assertIn("10.77.9.61", r.stderr)
        self.assertIn("cambox", r.stderr)

    def test_a_resolved_host_on_a_cambox_ip_is_dropped_and_named(self):
        stub = 'ndi_discovery_resolve_ipv4() { printf "10.77.9.63\\n"; }'
        r = _lib(f"{stub}\nndi_discovery_sender_ips")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), _pinned())
        self.assertIn("10.77.9.63", r.stderr)
        self.assertIn("cambox", r.stderr)

    def test_an_all_cambox_fleet_is_loud_never_an_empty_list(self):
        fleet = ("strih-lx|10.77.9.61|linux-genlock|always\nstream|10.77.9.62|windows-genlock|always\n"
                 "resolume|resolume.lan|windows-genlock|traveling")
        r = _lib('ndi_discovery_sender_ips pinned; echo "rc=$?"', env={"OBS_FLEET": fleet})
        self.assertIn("rc=1", r.stdout, r.stderr)
        self.assertNotIn("10.77.9.6", r.stdout)

    def test_cambox_ips_is_every_camera(self):
        r = _lib("ndi_discovery_cambox_ips")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), ",".join(_camera_ips()))
        self.assertGreaterEqual(len(_camera_ips()), 7)
        r = _bash(f'bash "{LIB}" --cambox-ips')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), ",".join(_camera_ips()))
        r = _lib('camera_resolve() { return 1; }\nndi_discovery_cambox_ips; echo "rc=$?"')
        self.assertIn("rc=1", r.stdout)
        self.assertIn("no camera", r.stderr)

    def test_list_common_helper(self):
        r = _lib('ndi_discovery_list_common "10.0.0.1, 10.0.0.2,10.0.0.3" "10.0.0.3,10.0.0.9,10.0.0.1"')
        self.assertEqual(r.stdout, "10.0.0.1,10.0.0.3")
        self.assertEqual(_lib('ndi_discovery_list_common "10.0.0.1" "10.0.0.2"').stdout, "")

    def test_list_common_sees_an_entry_with_a_port(self):
        r = _lib('ndi_discovery_list_common "10.0.0.1:5960, 10.0.0.2" "10.0.0.1"')
        self.assertEqual(r.stdout, "10.0.0.1:5960", "the entry is named as listed")

    def test_verdict_catches_a_cambox_entry_with_a_port(self):
        text = json.dumps({"ndi": {"networks": {"ips": _pinned() + ",10.77.9.61:5960"}}})
        r = _lib(f"ndi_discovery_config_verdict '{text}'")
        self.assertIn("cambox", r.stdout)
        self.assertIn("10.77.9.61:5960", r.stdout)

    def test_verdict_reads_ndi_networks_ips_never_another_ips_key(self):
        # A later "ips" string elsewhere in the file must not hide a cambox in ndi.networks.ips.
        text = json.dumps({"ndi": {"networks": {"ips": _pinned() + ",10.77.9.61"}},
                           "other": {"ips": _pinned()}})
        r = _lib(f"ndi_discovery_config_verdict '{text}'")
        self.assertTrue(r.stdout.startswith("FAIL:"), r.stdout)
        self.assertIn("cambox IP(s) 10.77.9.61", r.stdout)
        self.assertEqual(_lib(f"ndi_discovery_config_ips '{text}'").stdout, _pinned() + ",10.77.9.61")

    def test_verdict_fails_a_config_that_lists_a_cambox(self):
        text = json.dumps({"ndi": {"networks": {"ips": _pinned() + ",10.77.9.63"}}})
        r = _lib(f"ndi_discovery_config_verdict '{text}'")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(r.stdout.startswith("FAIL:"), r.stdout)
        self.assertIn("cambox", r.stdout)
        self.assertIn("10.77.9.63", r.stdout)
        self.assertIn("1389", r.stdout)

    def test_verdict_fails_the_issue_1342_list_a_box_still_carries(self):
        text = json.dumps({"ndi": {"networks": {"ips": ISSUE_1342_LIST}}})
        r = _lib(f"ndi_discovery_config_verdict '{text}'")
        self.assertTrue(r.stdout.startswith("FAIL:"), r.stdout)
        self.assertIn(",".join(_camera_ips()), r.stdout, "every listed cambox is named")

    def test_verdict_takes_an_explicit_forbidden_list(self):
        text = json.dumps({"ndi": {"networks": {"ips": "10.0.0.1,10.0.0.9"}}})
        r = _lib(f"ndi_discovery_config_verdict '{text}' 10.0.0.1 10.0.0.9")
        self.assertTrue(r.stdout.startswith("FAIL:"), r.stdout)
        self.assertIn("10.0.0.9", r.stdout)
        self.assertEqual(_lib(f"ndi_discovery_config_verdict '{text}' 10.0.0.1 ''").stdout.strip(), "ok")

    def test_verdict_fails_when_the_cambox_set_cannot_be_derived(self):
        text = json.dumps({"ndi": {"networks": {"ips": "10.77.9.202"}}})
        r = _lib(f"camera_resolve() {{ return 1; }}\nndi_discovery_config_verdict '{text}' 10.77.9.202")
        self.assertTrue(r.stdout.startswith("FAIL:"), r.stdout)
        self.assertIn("cambox", r.stdout)


class CamboxApply1389(unittest.TestCase):
    """The smallest safe cambox writer: the on-box program `--cambox-apply` prints rewrites ONLY
    /etc/ndi/ndi-config.v1.json with the SAME ndi_discovery_write_config STEP 7 calls, inside the
    read-only-root window, and restores ro (retried, loud). Run here with findmnt/mount/sync stubbed."""

    def _env(self, tmp):
        return {"NDI_DISCOVERY_SYSTEM_DIR": os.path.join(tmp, "etc-ndi"),
                "NDI_DISCOVERY_CAMBOX_DROPIN": os.path.join(tmp, "camera-box-ndi-discovery.conf")}

    def _run(self, tmp, root_opts="ro,relatime", ro_fails=0, ips=None, extra_stubs=""):
        log = os.path.join(tmp, "mount.log")
        env = self._env(tmp)
        lst = ips if ips is not None else _pinned()
        prog = _lib(f'ndi_discovery_cambox_apply_remote_snippet "{lst}"', env=env)
        self.assertEqual(prog.returncode, 0, prog.stderr)
        stubs = (
            f'findmnt() {{ printf "%s\\n" "{root_opts}"; }}\n'
            f'_ro_fails={ro_fails}\n'
            f'mount() {{ printf "%s\\n" "$*" >> "{log}"; '
            'if [ "$*" = "-o remount,ro /" ] && [ "$_ro_fails" -gt 0 ]; then _ro_fails=$((_ro_fails - 1)); return 32; fi; }\n'
            'sync() { :; }\nsleep() { :; }\n' + extra_stubs
        )
        # Fed on stdin to `bash -s`, exactly as the runbook's `ssh root@<cambox> bash -s < apply.sh` does.
        r = subprocess.run(["bash", "-s"], input=stubs + prog.stdout, capture_output=True, text=True, timeout=30)
        calls = _read(log).splitlines() if os.path.exists(log) else []
        return r, calls, os.path.join(tmp, "etc-ndi", "ndi-config.v1.json")

    def _seed(self, tmp, ips, dropin=True):
        env = self._env(tmp)
        self.assertEqual(_lib(f'ndi_discovery_write_config "$NDI_DISCOVERY_SYSTEM_DIR" "{ips}"', env=env).returncode, 0)
        if dropin:
            with open(env["NDI_DISCOVERY_CAMBOX_DROPIN"], "w") as fh:
                fh.write(_lib("ndi_discovery_dropin_content", env=env).stdout)

    def test_a_box_without_the_dropin_is_left_alone(self):
        # camera-box reads /etc/ndi only through the drop-in: without it the box is mDNS-only already.
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, ISSUE_1342_LIST, dropin=False)
            before = _read(os.path.join(tmp, "etc-ndi", "ndi-config.v1.json"))
            r, calls, path = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(calls, [], "no remount, no write to the stick")
            self.assertEqual(_read(path), before)
            self.assertIn("mDNS-only", r.stdout)
            self.assertNotIn("OK", r.stdout)

    def test_a_correct_list_with_other_keys_is_unchanged(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, _pinned())
            path = os.path.join(tmp, "etc-ndi", "ndi-config.v1.json")
            with open(path, "w") as fh:
                json.dump({"ndi": {"groups": {"recv": "Public"}, "networks": {"ips": _pinned()}}}, fh)
            r, calls, _ = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(calls, [], "the right list needs no write, whatever other keys the file has")
            self.assertIn("unchanged", r.stdout)

    def test_a_stray_discovery_key_is_still_rewritten(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, _pinned())
            path = os.path.join(tmp, "etc-ndi", "ndi-config.v1.json")
            with open(path, "w") as fh:
                json.dump({"ndi": {"networks": {"ips": _pinned(), "discovery": "10.77.9.200"}}}, fh)
            r, calls, _ = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertEqual(calls, ["-o remount,rw /", "-o remount,ro /"])
            self.assertEqual(json.loads(_read(path)), {"ndi": {"networks": {"ips": _pinned()}}})

    def test_ro_root_rewrites_the_config_inside_one_rw_window(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, ISSUE_1342_LIST)
            r, calls, path = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertEqual(calls, ["-o remount,rw /", "-o remount,ro /"])
            self.assertEqual(_read(path), _lib("ndi_discovery_config_json").stdout)
            self.assertEqual(sorted(os.listdir(os.path.dirname(path))), ["ndi-config.v1.json"],
                             "only the config file, no temp / backup left on the stick")
            self.assertIn("OK", r.stdout)

    def test_a_box_without_python3_keeps_the_step_7_backup(self):
        # The STEP 7 writer cannot merge without python3: canonical file + a backup of the old list.
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, ISSUE_1342_LIST)
            r, calls, path = self._run(tmp, extra_stubs="python3() { return 1; }\n")
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertEqual(calls, ["-o remount,rw /", "-o remount,ro /"])
            self.assertEqual(_read(path), _lib("ndi_discovery_config_json").stdout)
            baks = [f for f in os.listdir(os.path.dirname(path)) if f.startswith("ndi-config.v1.json.bak-")]
            self.assertEqual(len(baks), 1, os.listdir(os.path.dirname(path)))
            self.assertIn("10.77.9.61", _read(os.path.join(os.path.dirname(path), baks[0])))

    def test_a_failed_write_still_puts_the_root_back_read_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, ISSUE_1342_LIST)
            r, calls, path = self._run(tmp, extra_stubs="mktemp() { return 1; }\n")
            self.assertNotEqual(r.returncode, 0)
            self.assertEqual(calls, ["-o remount,rw /", "-o remount,ro /"], "the EXIT trap restores ro")
            self.assertIn("10.77.9.61", _read(path), "the old file is left as it was")
            self.assertNotIn("OK", r.stdout)

    def test_an_unchanged_config_writes_nothing_and_never_remounts(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, _pinned())
            before = os.stat(os.path.join(tmp, "etc-ndi", "ndi-config.v1.json")).st_mtime_ns
            r, calls, path = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(calls, [], "no remount for a no-op")
            self.assertEqual(os.stat(path).st_mtime_ns, before)
            self.assertIn("unchanged", r.stdout)

    def test_a_rw_root_is_never_remounted(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, ISSUE_1342_LIST)
            r, calls, path = self._run(tmp, root_opts="rw,relatime,errors=remount-ro")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(calls, [])
            self.assertEqual(_read(path), _lib("ndi_discovery_config_json").stdout)

    def test_a_transient_busy_ro_remount_is_retried(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, ISSUE_1342_LIST)
            r, calls, _ = self._run(tmp, ro_fails=2)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(calls.count("-o remount,ro /"), 3)

    def test_a_failed_ro_remount_is_loud_and_non_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, ISSUE_1342_LIST)
            r, calls, _ = self._run(tmp, ro_fails=99)
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("read-WRITE", r.stderr)
            self.assertNotIn("OK", r.stdout)

    def test_the_program_refuses_a_cambox_or_an_empty_list(self):
        for lst in ("10.77.9.202,10.77.9.61", ""):
            r = _lib(f'ndi_discovery_cambox_apply_remote_snippet "{lst}"; echo "rc=$?"')
            self.assertIn("rc=1", r.stdout, lst)
            self.assertNotIn("remount", r.stdout, lst)

    def test_the_program_embeds_the_step_7_writer(self):
        prog = _lib(f'ndi_discovery_cambox_apply_remote_snippet "{_pinned()}"').stdout
        self.assertIn("ndi_discovery_write_config ()", prog, "declare -f of the ONE writer, never a copy")
        self.assertIn('ndi_discovery_write_config "$_ndi_dir" "$_ndi_ips"', prog)

    def test_cli_prints_the_program_for_the_generated_list(self):
        r = _bash(f'bash "{LIB}" --cambox-apply pinned')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, _lib(f'ndi_discovery_cambox_apply_remote_snippet "{_pinned()}"').stdout)
        r = _bash(f'bash "{LIB}" --cambox-apply pinned', env={"OBS_FLEET": "stream|10.77.9.204|windows-genlock|always"})
        self.assertNotEqual(r.returncode, 0, "a generator failure never prints a program")
        self.assertEqual(r.stdout, "")


class LaptopScriptRun1389(unittest.TestCase):
    """RUN the real scripts/ndi-discovery-laptop.ps1 (the Windows writer for stream / resolume) through
    tests/pwsh/run_ndi_discovery_laptop_1389.sh. ubuntu-latest ships pwsh; dev1 has a portable one
    under ~/.local/pwsh74. A missing pwsh FAILS, never skips."""

    def test_the_real_ps1_removes_the_camboxes_and_keeps_the_rest(self):
        pwsh = os.environ.get("PWSH") or shutil.which("pwsh")
        home_pwsh = os.path.expanduser("~/.local/pwsh74/pwsh")
        if not pwsh and os.access(home_pwsh, os.X_OK):
            pwsh = home_pwsh
        if not pwsh:
            self.fail("no pwsh: install PowerShell 7 or set PWSH=/path/to/pwsh (the .ps1 must really run)")
        runner = os.path.join(REPO, "tests", "pwsh", "run_ndi_discovery_laptop_1389.sh")
        r = subprocess.run(["bash", runner], capture_output=True, text=True, timeout=240, cwd=REPO,
                           env=dict(os.environ, PWSH=pwsh))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("all cases ok", r.stdout)
        self.assertNotIn("FAIL", r.stdout)
