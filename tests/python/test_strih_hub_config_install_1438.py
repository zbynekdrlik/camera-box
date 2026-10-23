"""issue 1438 (review round 1): setup-strih step 13 installs the checkout's routing TOML only when the
INSTALLED hub loads it.

Step 13 keeps an already-installed hub binary (the hub is upgraded by its own deploy). The issue-1438
TOMLs put the cutters on the new `alsa` adapter, which a hub built before it refuses ("unknown adapter
'alsa'") -- after a deploy that re-ran setup-strih, the box's next hub restart would crash-loop and the
production interkom would be down. `strih_intercom_hub_config_install SRC DST BIN` therefore asks the
installed hub (`BIN --check-config --config SRC`): it installs on exit 0, keeps the installed file with
one WARNING on a refusal (and a binary from before the flag refuses an unknown flag), and installs on a
fresh box with no hub binary yet.

The real lib runs under the callers' `set -euo pipefail`; values reach bash as arguments only.
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "strih-provision.sh"
SETUP = REPO / "scripts" / "setup-strih.sh"


def _run(tmp_path, bin_body):
    src = tmp_path / "new.toml"
    src.write_text("adapter = \"alsa\"\n")
    dst = tmp_path / "etc" / "intercom.toml"
    hub = tmp_path / "intercom-hub"
    log = tmp_path / "hub-argv.log"
    if bin_body is not None:
        hub.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '" + str(log) + "'\n" + bin_body)
        hub.chmod(0o755)
    script = ('set -euo pipefail\n. "$1"\n'
              'strih_intercom_hub_config_install "$2" "$3" "$4"\necho "RC=$?"')
    r = subprocess.run(["/bin/bash", "-c", script, "harness", str(LIB), str(src), str(dst), str(hub)],
                       capture_output=True, text=True, timeout=60, env=dict(os.environ))
    return r, src, dst, log


def test_a_hub_that_loads_the_new_routing_gets_it_installed(tmp_path):
    r, src, dst, log = _run(tmp_path, "echo 'intercom-hub v: loads'\nexit 0\n")
    assert r.returncode == 0 and "RC=0" in r.stdout, r.stdout + r.stderr
    assert dst.read_text() == src.read_text()
    assert log.read_text().split() == ["--check-config", "--config", str(src)]
    assert "WARNING" not in r.stderr


def test_a_hub_that_refuses_it_keeps_the_installed_routing_with_one_warning(tmp_path):
    dst = tmp_path / "etc" / "intercom.toml"
    dst.parent.mkdir(parents=True)
    dst.write_text("adapter = \"pipewire\"\n")
    r, _src, dst, _log = _run(tmp_path, "echo \"unknown adapter 'alsa'\" >&2\nexit 1\n")
    assert r.returncode == 0 and "RC=0" in r.stdout, r.stdout + r.stderr
    assert dst.read_text() == "adapter = \"pipewire\"\n", "the old routing file is kept"
    assert r.stderr.count("WARNING") == 1, r.stderr
    assert "KEPT" in r.stderr and "unknown adapter 'alsa'" in r.stderr, r.stderr


def test_a_hub_from_before_the_flag_counts_as_a_refusal(tmp_path):
    dst = tmp_path / "etc" / "intercom.toml"
    dst.parent.mkdir(parents=True)
    dst.write_text("old\n")
    # clap's answer to an unknown flag: exit 2
    r, _src, dst, _log = _run(tmp_path, "echo \"error: unexpected argument '--check-config' found\" >&2\nexit 2\n")
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    assert dst.read_text() == "old\n"
    assert "KEPT" in r.stderr


def test_a_fresh_box_without_a_hub_binary_gets_the_routing(tmp_path):
    r, src, dst, log = _run(tmp_path, None)
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    assert dst.read_text() == src.read_text()
    assert not log.exists()


def test_a_refusing_hub_with_no_routing_installed_yet_still_gets_it_with_a_warning(tmp_path):
    r, src, dst, _log = _run(tmp_path, "exit 1\n")
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    assert dst.read_text() == src.read_text()
    assert "WARNING" in r.stderr and "reinstall the hub" in r.stderr


def test_setup_strih_step_13_installs_the_routing_through_the_guard():
    s = SETUP.read_text()
    assert len(s.splitlines()) <= 1000
    step13 = s[s.index('step 13 "'):s.index('step 14 "')]
    assert step13.count("strih_intercom_hub_config_install ") == 1
    assert ('strih_intercom_hub_config_install "${HERE}/../$(strih_lx_intercom_config)" '
            "/etc/intercom-hub/intercom.toml /usr/local/bin/intercom-hub") in step13
    assert "install -Dm644 \"${HERE}/../$(strih_lx_intercom_config)\"" not in step13
