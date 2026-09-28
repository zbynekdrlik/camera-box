"""Tier-0 tests for issue 1389: the NDI extra-IP list (`networks.ips`, issue 1342) names only the
obs-fleet `ndi-sender` hosts, NEVER a cambox.

A remote NDI finder with a cambox on its extra-IP list holds a TCP discovery connection into that
cambox's `:5960` listener, and libndi 6.3.2 intermittently aborts the whole camera-box process (an
uncaught `std::system_error` in its own `disc:recv` thread) when that connection closes. The camera
walk stays as the FORBIDDEN set: the generator drops a cambox IP by construction, the verdict FAILs a
config that lists one, and the Windows `.ps1` removes them.

ROZHODNUTÉ 5879261962 (supervisor, 28.9.2026): a cambox's OWN config carries NO `networks.ips` at
all -- the camboxes are mDNS-only receivers, so they open no outbound discovery connection either.
The cambox writer (setup-device STEP 7 and the `--cambox-apply` program) strips the list, keeps any
other key, and removes the config and the camera-box `NDI_CONFIG_DIR` drop-in when nothing else is
left; verify-device `(an)` FAILs a cambox that still lists any IP (`ndi_discovery_cambox_verdict`).

The shared helpers and the issue-1342 tests live in tests/python/test_ndi_discovery_1342.py; the
grader-behaviour cases for the cambox facet stay next to their harnesses there.
"""
import json
import os
import shlex
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

    def test_list_common_sees_a_mapped_address_and_ignores_bare_colon_forms(self):
        r = _lib('ndi_discovery_list_common "::ffff:10.0.0.1,10.0.0.2" "10.0.0.1"')
        self.assertEqual(r.stdout, "::ffff:10.0.0.1")
        # An entry that is only a port (or an IPv6 literal) never matches an EMPTY forbidden list.
        self.assertEqual(_lib('ndi_discovery_list_common ":5960,::1" ""').stdout, "")
        self.assertEqual(_lib('ndi_discovery_list_common "::1,fe80::1:5960" "10.0.0.1"').stdout, "")

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
        self.assertEqual(_lib(f"ndi_discovery_config_ips '{text}'").stdout, _pinned() + ",10.77.9.61\n")

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


class CamboxVerdict1389(unittest.TestCase):
    """ROZHODNUTÉ 5879261962: a cambox carries NO networks.ips at all (mDNS only). The verdict
    verify-device (an) grades a cambox with: its /etc/ndi config + its camera-box drop-in."""

    DROPIN = "[Service]\nEnvironment=NDI_CONFIG_DIR=/etc/ndi\n"

    def _verdict(self, conf, dropin):
        return _lib(f"ndi_discovery_cambox_verdict {shlex.quote(conf)} {shlex.quote(dropin)}")

    def test_no_config_and_no_dropin_is_ok(self):
        self.assertEqual(self._verdict("", "").stdout.strip(), "ok")

    def test_the_fleet_only_list_fails_naming_it(self):
        conf = json.dumps({"ndi": {"networks": {"ips": _pinned()}}})
        r = self._verdict(conf, self.DROPIN)
        self.assertTrue(r.stdout.startswith("FAIL:"), r.stdout)
        self.assertIn(_pinned(), r.stdout)
        self.assertIn("mDNS", r.stdout)

    def test_the_issue_1342_list_fails_naming_it(self):
        conf = json.dumps({"ndi": {"networks": {"ips": ISSUE_1342_LIST}}})
        r = self._verdict(conf, self.DROPIN)
        self.assertIn(ISSUE_1342_LIST, r.stdout)

    def test_a_list_without_the_dropin_still_fails(self):
        # Unread today, but a cambox carries no list at all: a later drop-in would load it again.
        conf = json.dumps({"ndi": {"networks": {"ips": _pinned()}}})
        self.assertTrue(self._verdict(conf, "").stdout.startswith("FAIL:"))

    def test_other_keys_without_a_list_are_ok(self):
        conf = json.dumps({"ndi": {"groups": {"recv": "Public"}, "networks": {}}})
        self.assertEqual(self._verdict(conf, self.DROPIN).stdout.strip(), "ok")

    def test_a_discovery_server_fails(self):
        conf = json.dumps({"ndi": {"networks": {"discovery": "10.77.9.200"}}})
        r = self._verdict(conf, self.DROPIN)
        self.assertTrue(r.stdout.startswith("FAIL:"), r.stdout)
        self.assertIn("discovery", r.stdout)

    def test_a_config_that_is_not_json_fails(self):
        r = self._verdict('{"ndi": {', self.DROPIN)
        self.assertTrue(r.stdout.startswith("FAIL:"), r.stdout)
        self.assertIn("JSON", r.stdout)

    def test_a_dropin_pointing_elsewhere_fails(self):
        r = self._verdict("", "[Service]\nEnvironment=NDI_CONFIG_DIR=/somewhere/else\n")
        self.assertTrue(r.stdout.startswith("FAIL:"), r.stdout)
        self.assertIn("/somewhere/else", r.stdout)

    def test_a_config_with_a_bom_is_json(self):
        # The plan reads the file as utf-8-sig; the verdict must not call the same file broken.
        conf = "\ufeff" + json.dumps({"ndi": {"groups": {"recv": "Public"}}})
        self.assertEqual(self._verdict(conf, self.DROPIN).stdout.strip(), "ok")
        listed = "\ufeff" + json.dumps({"ndi": {"networks": {"ips": _pinned()}}})
        r = self._verdict(listed, self.DROPIN)
        self.assertIn(_pinned(), r.stdout)
        self.assertNotIn("JSON", r.stdout)

    def test_a_non_string_list_still_fails(self):
        # The SDK documents a comma-separated string, but a hand-edited array is still a list.
        conf = json.dumps({"ndi": {"groups": {"recv": "Public"}, "networks": {"ips": ["10.77.9.61"]}}})
        r = self._verdict(conf, self.DROPIN)
        self.assertTrue(r.stdout.startswith("FAIL:"), r.stdout)
        self.assertIn("10.77.9.61", r.stdout)

    def test_a_blank_dropin_is_inert(self):
        # The same blank drop-in the plan removes as stale (CamboxApply1389) grades as no drop-in.
        self.assertEqual(self._verdict("", "  \n").stdout.strip(), "ok")

    def test_a_quoted_dropin_line_is_read_like_systemd_reads_it(self):
        dropin = '[Service]\nEnvironment="NDI_CONFIG_DIR=/etc/ndi"\n'
        self.assertEqual(_lib(f"ndi_discovery_dropin_config_dir {shlex.quote(dropin)}").stdout.strip(), "/etc/ndi")
        self.assertEqual(self._verdict("", dropin).stdout.strip(), "ok")


class CamboxApply1389(unittest.TestCase):
    """The cambox writer: the plan/apply pair setup-device STEP 7 calls, and the on-box program
    `--cambox-apply` prints around it (the read-only-root window). It strips networks.ips and
    networks.discovery, keeps every other key, and removes the config AND the camera-box drop-in when
    nothing else is left. Run here with findmnt/mount/sync/systemctl stubbed."""

    def _env(self, tmp):
        return {"NDI_DISCOVERY_SYSTEM_DIR": os.path.join(tmp, "etc-ndi"),
                "NDI_DISCOVERY_CAMBOX_DROPIN": os.path.join(tmp, "camera-box-ndi-discovery.conf")}

    def _paths(self, tmp):
        env = self._env(tmp)
        return os.path.join(env["NDI_DISCOVERY_SYSTEM_DIR"], "ndi-config.v1.json"), env["NDI_DISCOVERY_CAMBOX_DROPIN"]

    def _seed(self, tmp, conf=None, dropin=True):
        path, dropin_path = self._paths(tmp)
        if conf is not None:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(conf)
        if dropin:
            text = dropin if isinstance(dropin, str) else _lib("ndi_discovery_dropin_content", env=self._env(tmp)).stdout
            with open(dropin_path, "w") as fh:
                fh.write(text)
        return path, dropin_path

    def _canonical(self, tmp, ips):
        return _lib(f'ndi_discovery_config_json "{ips}"', env=self._env(tmp)).stdout

    def _run(self, tmp, root_opts="ro,relatime", ro_fails=0, extra_stubs=""):
        log = os.path.join(tmp, "mount.log")
        slog = os.path.join(tmp, "systemctl.log")
        prog = _lib("ndi_discovery_cambox_apply_remote_snippet", env=self._env(tmp))
        self.assertEqual(prog.returncode, 0, prog.stderr)
        stubs = (
            f'findmnt() {{ printf "%s\\n" "{root_opts}"; }}\n'
            f'_ro_fails={ro_fails}\n'
            f'mount() {{ printf "%s\\n" "$*" >> "{log}"; '
            'if [ "$*" = "-o remount,ro /" ] && [ "$_ro_fails" -gt 0 ]; then _ro_fails=$((_ro_fails - 1)); return 32; fi; }\n'
            f'systemctl() {{ printf "%s\\n" "$*" >> "{slog}"; }}\n'
            'sync() { :; }\nsleep() { :; }\n' + extra_stubs
        )
        # Fed on stdin to `bash -s`, exactly as the runbook's `ssh root@<cambox> bash -s < apply.sh` does.
        r = subprocess.run(["bash", "-s"], input=stubs + prog.stdout, capture_output=True, text=True, timeout=30)
        calls = _read(log).splitlines() if os.path.exists(log) else []
        sd = _read(slog).splitlines() if os.path.exists(slog) else []
        return r, calls, sd

    def _listing(self, path):
        return sorted(os.listdir(os.path.dirname(path))) if os.path.isdir(os.path.dirname(path)) else []

    def test_the_old_list_and_the_dropin_are_removed_in_one_rw_window(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, dropin = self._seed(tmp, self._canonical(tmp, ISSUE_1342_LIST))
            r, calls, sd = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertEqual(calls, ["-o remount,rw /", "-o remount,ro /"])
            self.assertFalse(os.path.exists(path), "the config is gone")
            self.assertFalse(os.path.exists(dropin), "the NDI_CONFIG_DIR drop-in is gone")
            self.assertEqual(self._listing(path), [], "no backup of the lib's own file")
            self.assertEqual(sd, ["daemon-reload"], "the removed drop-in is unloaded")
            self.assertIn("OK", r.stdout)

    def test_the_fleet_only_list_is_removed_too(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, dropin = self._seed(tmp, self._canonical(tmp, _pinned()))
            r, calls, _ = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertFalse(os.path.exists(path))
            self.assertFalse(os.path.exists(dropin))

    def test_other_keys_survive_with_the_dropin(self):
        with tempfile.TemporaryDirectory() as tmp:
            conf = json.dumps({"ndi": {"groups": {"recv": "Public"},
                                       "networks": {"ips": ISSUE_1342_LIST, "discovery": "10.77.9.200"}},
                               "other": 1})
            path, dropin = self._seed(tmp, conf)
            r, calls, sd = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertEqual(calls, ["-o remount,rw /", "-o remount,ro /"])
            self.assertEqual(json.loads(_read(path)), {"ndi": {"groups": {"recv": "Public"}}, "other": 1})
            self.assertTrue(os.path.exists(dropin), "the other keys still matter, so the drop-in stays")
            self.assertEqual(sd, [])

    def test_an_mdns_only_box_is_left_alone(self):
        with tempfile.TemporaryDirectory() as tmp:
            r, calls, sd = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual((calls, sd), ([], []), "no remount, no write to the stick")
            self.assertIn("nothing written", r.stdout)

    def test_a_config_with_only_other_keys_is_left_alone(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, dropin = self._seed(tmp, json.dumps({"ndi": {"groups": {"recv": "Public"}}}))
            before = _read(path)
            r, calls, _ = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(calls, [])
            self.assertEqual(_read(path), before)
            self.assertTrue(os.path.exists(dropin))

    def test_a_lone_dropin_is_removed(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, dropin = self._seed(tmp, None)
            r, calls, sd = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertEqual(calls, ["-o remount,rw /", "-o remount,ro /"])
            self.assertFalse(os.path.exists(dropin))
            self.assertEqual(sd, ["daemon-reload"])

    def test_a_blank_dropin_is_stale_and_removed(self):
        # verify-device (an) grades a blank drop-in as no drop-in; the writer must not refuse it.
        with tempfile.TemporaryDirectory() as tmp:
            path, dropin = self._seed(tmp, self._canonical(tmp, ISSUE_1342_LIST), dropin="  \n")
            r, calls, sd = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertNotIn("REFUSED", r.stderr)
            self.assertFalse(os.path.exists(path))
            self.assertFalse(os.path.exists(dropin))
            self.assertEqual(sd, ["daemon-reload"])

    def test_a_lone_blank_dropin_is_removed(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = self._env(tmp)
            _, dropin = self._seed(tmp, None, dropin="\n")
            self.assertEqual(
                _lib('ndi_discovery_cambox_plan "$NDI_DISCOVERY_SYSTEM_DIR" "$NDI_DISCOVERY_CAMBOX_DROPIN"',
                     env=env).stdout.strip(), "remove")
            r, calls, sd = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertFalse(os.path.exists(dropin))
            self.assertEqual(sd, ["daemon-reload"])

    def test_a_leftover_list_without_the_dropin_is_removed(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, _ = self._seed(tmp, self._canonical(tmp, _pinned()), dropin=False)
            r, calls, sd = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertEqual(calls, ["-o remount,rw /", "-o remount,ro /"])
            self.assertFalse(os.path.exists(path))
            self.assertEqual(sd, [], "no drop-in was there, so nothing to unload")

    def test_a_second_run_writes_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, self._canonical(tmp, ISSUE_1342_LIST))
            first, _, _ = self._run(tmp)
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
            os.remove(os.path.join(tmp, "mount.log"))
            os.remove(os.path.join(tmp, "systemctl.log"))
            again, calls, sd = self._run(tmp)
            self.assertEqual(again.returncode, 0, again.stderr)
            self.assertEqual((calls, sd), ([], []), "the second run never remounts")
            self.assertIn("nothing written", again.stdout)

    def test_a_bom_config_with_the_list_is_stripped_keeping_other_keys(self):
        with tempfile.TemporaryDirectory() as tmp:
            conf = "\ufeff" + json.dumps({"ndi": {"groups": {"recv": "Public"}, "networks": {"ips": _pinned()}}})
            path, dropin = self._seed(tmp, conf)
            r, _, _ = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertEqual(json.loads(_read(path)), {"ndi": {"groups": {"recv": "Public"}}})
            self.assertTrue(os.path.exists(dropin))

    def test_a_non_string_list_is_stripped_keeping_other_keys(self):
        with tempfile.TemporaryDirectory() as tmp:
            conf = json.dumps({"ndi": {"groups": {"recv": "Public"}, "networks": {"ips": ["10.77.9.61"]}}})
            path, dropin = self._seed(tmp, conf)
            r, calls, _ = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertEqual(calls, ["-o remount,rw /", "-o remount,ro /"])
            self.assertEqual(json.loads(_read(path)), {"ndi": {"groups": {"recv": "Public"}}})
            self.assertTrue(os.path.exists(dropin))

    def test_a_quoted_dropin_line_is_ours(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = self._env(tmp)
            quoted = f'[Service]\nEnvironment="NDI_CONFIG_DIR={env["NDI_DISCOVERY_SYSTEM_DIR"]}"\n'
            path, dropin = self._seed(tmp, self._canonical(tmp, ISSUE_1342_LIST), dropin=quoted)
            r, _, _ = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertFalse(os.path.exists(path))
            self.assertFalse(os.path.exists(dropin))

    def test_a_foreign_dropin_is_refused_and_nothing_touched(self):
        with tempfile.TemporaryDirectory() as tmp:
            foreign = "[Service]\nEnvironment=NDI_CONFIG_DIR=/somewhere/else\n"
            path, dropin = self._seed(tmp, self._canonical(tmp, ISSUE_1342_LIST), dropin=foreign)
            before = _read(path)
            r, calls, sd = self._run(tmp)
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("REFUSED", r.stderr)
            self.assertIn("/somewhere/else", r.stderr)
            self.assertEqual((calls, sd), ([], []))
            self.assertEqual(_read(path), before)
            self.assertEqual(_read(dropin), foreign)

    def test_a_broken_json_file_is_removed_with_a_backup(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, dropin = self._seed(tmp, '{"ndi": {"networks": {"ips": "10.77.9.61"}}')  # a brace short
            r, calls, _ = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertFalse(os.path.exists(path))
            self.assertFalse(os.path.exists(dropin))
            baks = [f for f in self._listing(path) if f.startswith("ndi-config.v1.json.bak-")]
            self.assertEqual(len(baks), 1, self._listing(path))

    def test_without_python3_the_libs_own_file_goes_without_a_backup(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, dropin = self._seed(tmp, self._canonical(tmp, ISSUE_1342_LIST))
            r, _, _ = self._run(tmp, extra_stubs="python3() { return 1; }\n")
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertFalse(os.path.exists(path))
            self.assertEqual(self._listing(path), [])

    def test_without_python3_a_foreign_file_is_backed_up_first(self):
        with tempfile.TemporaryDirectory() as tmp:
            conf = json.dumps({"ndi": {"groups": {"recv": "Public"}, "networks": {"ips": "10.77.9.61"}}})
            path, dropin = self._seed(tmp, conf)
            r, _, _ = self._run(tmp, extra_stubs="python3() { return 1; }\n")
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertFalse(os.path.exists(path))
            baks = [f for f in self._listing(path) if f.startswith("ndi-config.v1.json.bak-")]
            self.assertEqual(len(baks), 1)
            self.assertEqual(_read(os.path.join(os.path.dirname(path), baks[0])), conf)

    def test_without_python3_a_config_without_a_list_is_left_alone(self):
        # No python3 on the box: other keys cannot be kept by a rewrite, but a config that lists
        # nothing needs no change at all -- never delete it (the backup would be the only copy).
        with tempfile.TemporaryDirectory() as tmp:
            conf = json.dumps({"ndi": {"groups": {"recv": "Public"}}})
            path, dropin = self._seed(tmp, conf)
            r, calls, sd = self._run(tmp, extra_stubs="python3() { return 1; }\n")
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertEqual((calls, sd), ([], []))
            self.assertEqual(_read(path), conf)
            self.assertTrue(os.path.exists(dropin))
            self.assertIn("nothing written", r.stdout)

    def test_a_rw_root_is_never_remounted(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, _ = self._seed(tmp, self._canonical(tmp, ISSUE_1342_LIST))
            r, calls, _ = self._run(tmp, root_opts="rw,relatime,errors=remount-ro")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(calls, [])
            self.assertFalse(os.path.exists(path))

    def test_a_transient_busy_ro_remount_is_retried(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, self._canonical(tmp, ISSUE_1342_LIST))
            r, calls, _ = self._run(tmp, ro_fails=2)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(calls.count("-o remount,ro /"), 3)

    def test_a_failed_ro_remount_is_loud_and_non_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._seed(tmp, self._canonical(tmp, ISSUE_1342_LIST))
            r, _, _ = self._run(tmp, ro_fails=99)
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("read-WRITE", r.stderr)
            self.assertNotIn("OK", r.stdout)

    def test_a_failed_strip_still_puts_the_root_back_read_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            conf = json.dumps({"ndi": {"groups": {"recv": "Public"}, "networks": {"ips": "10.77.9.61"}}})
            path, _ = self._seed(tmp, conf)
            r, calls, _ = self._run(tmp, extra_stubs="mktemp() { return 1; }\n")
            self.assertNotEqual(r.returncode, 0)
            self.assertEqual(calls, ["-o remount,rw /", "-o remount,ro /"], "the EXIT trap restores ro")
            self.assertEqual(_read(path), conf, "the file is left as it was")
            self.assertNotIn("OK", r.stdout)

    def test_the_program_embeds_the_shared_plan_and_never_the_list_writer(self):
        prog = _lib("ndi_discovery_cambox_apply_remote_snippet").stdout
        self.assertIn("ndi_discovery_cambox_plan ()", prog, "declare -f of the ONE plan, never a copy")
        self.assertIn("ndi_discovery_cambox_apply_plan ()", prog)
        self.assertNotIn("ndi_discovery_write_config", prog, "a cambox never gets a list written")

    def test_cli_prints_the_program_and_needs_no_fleet_list(self):
        r = _bash(f'bash "{LIB}" --cambox-apply')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, _lib("ndi_discovery_cambox_apply_remote_snippet").stdout)
        r = _bash(f'bash "{LIB}" --cambox-apply', env={"OBS_FLEET": "stream|10.77.9.204|windows-genlock|always"})
        self.assertEqual(r.returncode, 0, "the cambox program does not depend on the OBS-fleet list")


class SetupDeviceStep7Run1389(unittest.TestCase):
    """Run the REAL STEP 7 NDI block (sliced from setup-device.sh) against temp paths: the plan's
    none / apply / refuse branches and a failed apply, each with the message the operator sees."""

    def _block(self):
        text = _read(os.path.join(REPO, "scripts", "setup-device.sh"))
        start = text.find('NDI_PLAN="$(ndi_discovery_cambox_plan')
        self.assertGreaterEqual(start, 0, "STEP 7 plan line")
        end = text.find("\nesac\n", start)
        self.assertGreater(end, start, "the STEP 7 case block ends")
        return text[start:end + len("\nesac\n")]

    def _env(self, tmp):
        return {"NDI_DISCOVERY_SYSTEM_DIR": os.path.join(tmp, "etc-ndi"),
                "NDI_DISCOVERY_CAMBOX_DROPIN": os.path.join(tmp, "camera-box-ndi-discovery.conf")}

    def _run(self, tmp, extra=""):
        prelude = 'fail() { echo "FAIL $1"; exit 1; }\n' + extra
        return _bash(f'set -euo pipefail\n. "{LIB}"\n{prelude}{self._block()}', env=self._env(tmp))

    def _seed(self, tmp, conf, dropin_text=None):
        env = self._env(tmp)
        os.makedirs(env["NDI_DISCOVERY_SYSTEM_DIR"], exist_ok=True)
        path = os.path.join(env["NDI_DISCOVERY_SYSTEM_DIR"], "ndi-config.v1.json")
        with open(path, "w") as fh:
            fh.write(conf)
        if dropin_text is None:
            dropin_text = _lib("ndi_discovery_dropin_content", env=env).stdout
        with open(env["NDI_DISCOVERY_CAMBOX_DROPIN"], "w") as fh:
            fh.write(dropin_text)
        return path, env["NDI_DISCOVERY_CAMBOX_DROPIN"]

    def test_a_clean_box_is_left_alone(self):
        with tempfile.TemporaryDirectory() as tmp:
            r = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertIn("no networks.ips on this cambox", r.stdout)
            self.assertIn("nothing to change", r.stdout)

    def test_the_old_list_is_taken_off(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, dropin = self._seed(tmp, json.dumps({"ndi": {"networks": {"ips": ISSUE_1342_LIST}}}))
            r = self._run(tmp)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertIn("NDI receiver config: remove", r.stdout)
            self.assertFalse(os.path.exists(path))
            self.assertFalse(os.path.exists(dropin))

    def test_a_foreign_dropin_fails_loud_and_touches_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            conf = json.dumps({"ndi": {"networks": {"ips": ISSUE_1342_LIST}}})
            foreign = "[Service]\nEnvironment=NDI_CONFIG_DIR=/somewhere/else\n"
            path, dropin = self._seed(tmp, conf, foreign)
            r = self._run(tmp)
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("FAIL NDI receiver config:", r.stdout)
            self.assertIn("/somewhere/else", r.stdout)
            self.assertEqual(_read(path), conf)
            self.assertEqual(_read(dropin), foreign)

    def test_a_failed_apply_fails_loud(self):
        with tempfile.TemporaryDirectory() as tmp:
            conf = json.dumps({"ndi": {"groups": {"recv": "Public"}, "networks": {"ips": ISSUE_1342_LIST}}})
            path, _ = self._seed(tmp, conf)
            r = self._run(tmp, extra="mktemp() { return 1; }\n")
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("FAIL NDI receiver config cleanup (strip) failed", r.stdout)
            self.assertEqual(_read(path), conf)


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
