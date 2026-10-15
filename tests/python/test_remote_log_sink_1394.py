"""Issue 1394 (owner ROZHODNUTE 9.10.2026): the cambox off-box log sink is the box's OWN venue strih.

Live 8.10.2026: `scripts/lib/remote-logging.sh` hard-coded `REMOTE_LOG_DEV1_IP=10.77.9.200`, a DHCP pool
address leased to a phone, so systemd-journal-upload failed on all seven camboxes and netconsole could
not arm, while verify-device (ak) passed because it graded against the same constant. The owner ruled:
no rig service and no rig data on dev1; the sink is the strih notebook of the venue.

These tests pin the cambox side:
  * remote_log_sink_ip CAMERA derives the sink from the venue fact files (the STRIH_IP of the strih-role
    box whose STRIH_CAMERAS lists the camera), never a literal; none / two / a bad address fail loud;
  * the generators and the verdict take the sink as an argument and refuse / fail closed without one;
  * setup-device resolves the sink right after the box name and writes the files with it, verify-device
    (ak) grades against the box's own sink, create-usb bakes no box-specific logging file;
  * the dev1 receiver installer is gone, with every reference to it.
"""

import os
import pathlib
import re
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib" / "remote-logging.sh"
SETUP = REPO / "scripts" / "setup-device.sh"
VERIFY = REPO / "scripts" / "verify-device.sh"
CREATE_USB = REPO / "scripts" / "create-usb-linux.sh"
BOXES = REPO / "scripts" / "strih-boxes"


def _bash(body, *args, env=None, strict=True):
    """Run BODY with the lib sourced (under the callers' set -euo pipefail when STRICT); args are $1..."""
    head = "set -euo pipefail\n" if strict else "set -uo pipefail\n"
    e = dict(os.environ)
    e.pop("STRIH_BOXES_DIR", None)
    if env:
        e.update(env)
    return subprocess.run(["bash", "-c", head + '. "$LIB"\n' + body, "harness", *args],
                          capture_output=True, text=True, timeout=60, env=dict(e, LIB=str(LIB)))


def _fact_file(path, name, ip, cams, role="strih", venue="snv"):
    text = (BOXES / "strih-snv.env").read_text()
    repl = {"STRIH_HOSTNAME": name, "STRIH_IP": ip, "STRIH_CAMERAS": cams, "STRIH_ROLE": role,
            "STRIH_VENUE": venue, "STRIH_NDI_PREFIX": name.upper()}
    out = []
    for line in text.splitlines():
        key = line.split("=", 1)[0]
        out.append(f"{key}={repl[key]}" if key in repl and "=" in line and not line.startswith("#") else line)
    (path / f"{name}.env").write_text("\n".join(out) + "\n")


# --- the resolver ---------------------------------------------------------------------------------------

def _fact(name, key):
    for line in (BOXES / f"{name}.env").read_text().splitlines():
        if line.startswith(key + "="):
            return line.split("=", 1)[1]
    raise AssertionError(f"{name}.env has no {key}")


def test_every_camera_resolves_to_its_own_venue_strih_from_the_fact_files():
    for box in ("strih-snv", "strih-pp"):
        assert _fact(box, "STRIH_ROLE") == "strih"
        for n in _fact(box, "STRIH_CAMERAS").split():
            r = _bash('remote_log_sink_ip "$1"', f"cam{n}")
            assert r.returncode == 0, r.stderr
            assert r.stdout == _fact(box, "STRIH_IP") + "\n", (n, r.stdout)


def test_the_live_fleet_today():
    want = {"cam1": "10.77.8.202", "cam2": "10.77.9.202", "cam3": "10.77.9.202", "cam4": "10.77.8.202",
            "cam5": "10.77.9.202", "cam6": "10.77.8.202", "cam7": "10.77.8.202"}
    for cam, ip in want.items():
        assert _bash('remote_log_sink_ip "$1"', cam).stdout.strip() == ip, cam


def test_no_dev1_address_is_left_in_the_lib():
    text = LIB.read_text()
    assert "10.77.9.200" not in text
    assert "REMOTE_LOG_DEV1_IP" not in text and "REMOTE_LOG_JOURNAL_URL" not in text
    assert not re.search(r"(?m)^[^#]*\b10\.7[67]\.\d+\.\d+\b", text), "no literal rig address in the lib's code"


def test_a_camera_no_venue_lists_or_a_bad_name_has_no_sink():
    for cam in ("cam8", "cam0", "cam01", "CAM2", "strih-snv", "", "cam2x"):
        r = _bash('remote_log_sink_ip "$1"', cam)
        assert r.returncode == 1 and r.stdout == "", (cam, r.stdout)
        assert "remote-logging:" in r.stderr and "issue 1394" in r.stderr, r.stderr


def test_two_venues_listing_one_camera_refuse(tmp_path):
    _fact_file(tmp_path, "strih-aa", "10.0.0.1", "2 3")
    _fact_file(tmp_path, "strih-bb", "10.0.1.1", "3 4")
    r = _bash('remote_log_sink_ip "$1"', "cam3", env={"STRIH_BOXES_DIR": str(tmp_path)})
    assert r.returncode == 1 and r.stdout == ""
    assert "two strih fact files" in r.stderr and "strih-aa.env" in r.stderr and "strih-bb.env" in r.stderr
    r = _bash('remote_log_sink_ip "$1"', "cam4", env={"STRIH_BOXES_DIR": str(tmp_path)})
    assert r.stdout == "10.0.1.1\n", r.stderr


def test_a_stream_role_box_is_never_a_sink(tmp_path):
    _fact_file(tmp_path, "stream-aa", "10.0.0.9", "5", role="stream")
    r = _bash('remote_log_sink_ip "$1"', "cam5", env={"STRIH_BOXES_DIR": str(tmp_path)})
    assert r.returncode == 1 and "no strih fact file" in r.stderr, r.stderr


def test_a_bad_sink_address_or_an_unparseable_fact_file_refuses(tmp_path):
    _fact_file(tmp_path, "strih-aa", "10.0.0.300", "2")
    r = _bash('remote_log_sink_ip "$1"', "cam2", env={"STRIH_BOXES_DIR": str(tmp_path)})
    assert r.returncode == 1 and "not an IPv4 address" in r.stderr, r.stderr
    (tmp_path / "strih-aa.env").write_text("STRIH_IP=10.0.0.1\nthis is not a fact\n")
    r = _bash('remote_log_sink_ip "$1"', "cam2", env={"STRIH_BOXES_DIR": str(tmp_path)})
    assert r.returncode == 1 and "does not parse" in r.stderr, r.stderr


def test_the_resolver_leaves_the_callers_shell_alone_and_survives_noglob():
    # setup-device / verify-device run under set -euo pipefail; the version gates build specs under -f.
    r = _bash('set -f\nshopt -s failglob\nx="$(remote_log_sink_ip cam5)"\necho "sink=$x"\n'
              'declare -F strih_box_parse_file >/dev/null && echo LEAKED || echo clean\n'
              '[[ $- == *f* ]] && echo noglob-kept')
    assert r.returncode == 0, r.stderr
    assert r.stdout.splitlines() == ["sink=10.77.9.202", "clean", "noglob-kept"], r.stdout


# --- the generators and the verdict take the sink --------------------------------------------------------

def test_the_generators_bake_the_given_sink_and_refuse_none():
    r = _bash('remote_log_netconsole_setup_script_content "$1"', "10.77.8.202")
    assert r.returncode == 0, r.stderr
    assert 'SINK_IP="10.77.8.202"' in r.stdout and "DEV1" not in r.stdout and "dev1" not in r.stdout
    r2 = subprocess.run(["bash", "-n"], input=r.stdout, capture_output=True, text=True)
    assert r2.returncode == 0, r2.stderr
    r = _bash('remote_log_journal_upload_conf_content "$1"', "10.77.8.202")
    assert r.stdout == "[Upload]\nURL=http://10.77.8.202:19532\n", r.stdout
    for fn in ("remote_log_netconsole_setup_script_content", "remote_log_journal_upload_conf_content"):
        for bad in ("", "10.77.9", "dev1.lan", "10.77.9.256"):
            r = _bash(f'{fn} "$1"', bad, strict=False)
            assert r.returncode == 1 and r.stdout == "", (fn, bad, r.stdout)
            assert "issue 1394" in r.stderr


GREEN = ("NC_SVC_ENABLED=enabled\nNC_SVC_ACTIVE=active\nNC_SVC_RESULT=success\nNC_SCRIPT_X=yes\n"
         "NC_ENABLED=1\nNC_REMOTE_IP=10.77.9.202\nNC_REMOTE_PORT=514\nJU_SVC_ENABLED=enabled\n"
         "JU_URL=http://10.77.9.202:19532\nJU_STATE_SAVE=--save-state=/run/systemd/journal-upload/state")


def test_the_verdict_grades_against_the_given_sink_and_fails_closed_without_one():
    assert _bash('remote_log_verdict "$1" "$2"', GREEN, "10.77.9.202").stdout == "ok\n"
    v = _bash('remote_log_verdict "$1" "$2"', GREEN, "10.77.8.202").stdout
    assert "remote_ip=10.77.9.202 != the venue strih sink 10.77.8.202" in v, v
    assert "URL=http://10.77.9.202:19532 != http://10.77.8.202:19532" in v, v
    for sink in ("", "nope"):
        v = _bash('remote_log_verdict "$1" "$2"', GREEN, sink).stdout
        assert v.startswith("FAIL: no log sink resolved for this box"), v
        assert "ok" != v.strip()
    old_dev1 = GREEN.replace("10.77.9.202", "10.77.9.200")
    v = _bash('remote_log_verdict "$1" "$2"', old_dev1, "10.77.9.202").stdout
    assert "10.77.9.200" in v and v.count("FAIL:") == 2, v


# --- the consumers ----------------------------------------------------------------------------------------

def test_setup_device_resolves_the_sink_right_after_the_box_name_and_writes_with_it():
    s = SETUP.read_text()
    name = s.index('\nresolve_device_name "$DEVICE_NAME_ARG"')
    sink = s.index('REMOTE_LOG_SINK_IP="$(remote_log_sink_ip "$CAMERA_NAME")"')
    assert name < sink < s.index("\nensure_root_writable\n") < s.index("# STEP 1: Set hostname")
    assert "|| fail \"no venue log sink for" in s[sink:sink + 300]
    assert 'remote_log_netconsole_setup_script_content "$REMOTE_LOG_SINK_IP" > "$REMOTE_LOG_NC_SCRIPT_PATH"' in s
    assert 'remote_log_journal_upload_conf_content "$REMOTE_LOG_SINK_IP" > "$REMOTE_LOG_JU_CONF_PATH"' in s
    assert "REMOTE_LOG_DEV1_IP" not in s and "REMOTE_LOG_JOURNAL_URL" not in s


def test_verify_device_ak_grades_against_the_boxs_own_sink():
    s = VERIFY.read_text()
    ak = s.index("\n# (ak) off-box remote logging")
    block = s[ak:s.index("\n# (", ak + 10)]
    assert 'REMOTELOG_SINK="$(remote_log_sink_ip "$CAMERA_NAME")"' in block
    assert 'remote_log_verdict "$REMOTELOG_BLOCK" "$REMOTELOG_SINK"' in block
    assert "dev1:" not in block and "REMOTE_LOG_JOURNAL_URL" not in block


def test_create_usb_bakes_no_box_specific_logging_file():
    s = CREATE_USB.read_text()
    code = "\n".join(l for l in s.splitlines() if not l.lstrip().startswith("#"))
    for word in ("remote_log_", "REMOTE_LOG_", "systemctl enable cambox-netconsole",
                 "systemctl enable systemd-journal-upload", "lib/remote-logging.sh"):
        assert word not in code, word
    assert "systemd-journal-remote" in code, "the package that ships the uploader stays in the image"


def test_the_dev1_receiver_installer_is_gone_with_every_reference():
    assert not (REPO / "scripts" / "dev1-remote-log-install.sh").exists()
    hits = []
    for top in ("scripts", "tests", ".claude", "systemd"):
        for p in (REPO / top).rglob("*"):
            if p.is_file() and p.suffix in (".sh", ".py", ".rs", ".md", ".service", ".yml", ".txt"):
                if p.name == "test_remote_log_sink_1394.py":
                    continue
                if "dev1-remote-log-install" in p.read_text(errors="replace"):
                    hits.append(str(p.relative_to(REPO)))
    if "dev1-remote-log-install" in (REPO / "CLAUDE.md").read_text():
        hits.append("CLAUDE.md")
    assert hits == [], hits
