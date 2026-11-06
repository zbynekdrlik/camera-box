"""Issue 1485, ROZHODNUTE 6628785864 (owner 4.11.2026): the intercom hub and the OBS dante-input on
strih-snv follow Dante PTP IN PROCESS (dstack v0.6.23 `ClockSource::Linked`: dsync's follower on UDP
319/320); the dsync service stays the only writer of the system clock.

Pinned here (scripts/lib/strih-dante-ptp.sh + the hub's rows in strih-intercom-hub-unit.sh):
  * the pin threshold: a component is READY when its release pin's DSTACK_VERSION >= v0.6.23;
  * the OBS's right to bind 319: the sysctl drop-in `net.ipv4.ip_unprivileged_port_start = 319` (design
    6629112890), installed + set live on a Dante-program box, removed (ours only) elsewhere, graded;
  * the media clock row on both devices: the LAST `media clock source source=<X>` line, Internal = OK,
    Service / None / no line = FAIL once READY, a NOTE below; the follower's `did not start` reason named;
  * verify item 16: exactly CAP_NET_BIND_SERVICE (ambient + bounding set, no file capability) once the
    intercom pin is READY, FAIL when missing or another capability appears, a NOTE below the threshold;
  * the wiring (step 13, item 9, item 16).
The leg filter's PTP rule is pinned in test_vban_leg_1439.py.
"""
import os
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-dante-ptp.sh"
HUB_UNIT = SCRIPTS / "lib" / "strih-intercom-hub-unit.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
LEAKY = ("STRIH_DANTE_RX_PIN_ROOT", "STRIH_DANTE_RX_PIN_OWNER", "STRIH_DANTE_PTP_INTERCOM_PIN",
         "STRIH_DANTE_PTP_OBS_PIN", "STRIH_HUB_AMBIENT_CAPS", "STRIH_HUB_BOUNDING_CAPS", "STRIH_HUB_JOURNAL_FILE",
         "STRIH_BOXES_DIR", "STRIH_LX_IP", "STRIH_NIC_IFACE", "STRIH_HUB_INVOCATION_ID", "STRIH_HUB_JOURNAL_EUID",
         "STRIH_HUB_JOURNAL_GROUPS")
ROWS = 'ok() { echo "OK $*"; }; bad() { echo "BAD $*"; }; note() { echo "NOTE $*"; }\n'

# the dstack lines as the obs release forwards them into the OBS log (the obs project, 4.11.2026)
INTERNAL = "12:00:01.000: dante-input dlink:  INFO dstack::clock::linked: media clock source source=Internal from=Service"
SERVICE = "12:00:00.500: dante-input dlink:  INFO dstack::clock::linked: media clock source source=Service from=None"
NOT_STARTED = ('12:00:00.400: dante-input dlink:  WARN dstack::clock::linked: media clock: dsync\'s in-process follower '
               'did not start: the dsync service\'s clock is used iface="enp7s0.70" '
               'reason="port 319 needs CAP_NET_BIND_SERVICE or root" error=permission denied')
NO_SOURCE = "12:00:02.000: dante-input dlink:  WARN dstack::clock::linked: media clock: no source ready from=Internal"

DROPIN = ("# issue 1485: written by camera-box setup-strih step 13 on a Dante-program box; every run rewrites it.\n"
          "# The strih OBS dante-input (a desktop-user process under systemd --user) follows Dante PTP in process\n"
          "# and binds UDP 319 (ROZHODNUTE 6628785864). Every unprivileged process may bind 319-1023 on this box.\n"
          "net.ipv4.ip_unprivileged_port_start = 319\n")


def _bash(body, *args, env=None, stdin=None, lib=LIB):
    """BODY under the callers' `set -euo pipefail`, values as $1.. (never in the text)."""
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    script = 'set -euo pipefail\n. "%s"\n%s%s' % (lib, ROWS, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60, input=stdin)


def _pins(tmp, intercom="v0.6.23", obs="v0.6.23"):
    ic, ob = tmp / "intercom.pin", tmp / "obs.pin"
    ic.write_text("INTERCOM_RELEASE_TAG=v9.9.9\nINTERCOM_RELEASE_DSTACK_VERSION=%s\n" % intercom)
    ob.write_text("OBS_RELEASE_TAG=v9.9.9\nOBS_RELEASE_DSTACK_VERSION=%s\n" % obs)
    return {"STRIH_DANTE_PTP_INTERCOM_PIN": str(ic), "STRIH_DANTE_PTP_OBS_PIN": str(ob)}


# --- the pin threshold -----------------------------------------------------------------------------------

@pytest.mark.parametrize("ver,ready", [("v0.6.23", True), ("v0.6.24", True), ("v0.7.0", True), ("v1.0.0", True),
                                       ("v0.6.22", False), ("v0.6.19", False), ("v0.5.99", False), ("", False),
                                       ("garbage", False), ("v0.6.2x", False)])
def test_a_component_is_ready_from_dstack_v0_6_23_on(tmp_path, ver, ready):
    r = _bash('if strih_dante_ptp_pin_ready obs; then echo READY; else echo BELOW; fi', env=_pins(tmp_path, obs=ver))
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == ("READY" if ready else "BELOW")


def test_dtray_and_the_ptp_threshold_share_the_one_version_rule():
    """Review round 1: one vX.Y.Z order (release_version_cmp), never a copied comparator."""
    for lib in ("strih-dante-ptp.sh", "dtray.sh"):
        assert "release_version_cmp" in (SCRIPTS / "lib" / lib).read_text(), lib
    r = _bash('. scripts/lib/dtray.sh\nfor p in "v0.6.13 v0.6.13" "v0.6.19 v0.6.13" "v0.6.10 v0.6.13" "x v0.6.13"; do '
              'set -- $p; if dtray_version_ge "$1" "$2"; then echo 0; else echo $?; fi; done')
    assert r.stdout.split() == ["0", "0", "1", "2"], r.stdout + r.stderr


def test_the_real_pins_are_read():
    for comp, key in (("intercom", "INTERCOM_RELEASE_DSTACK_VERSION"), ("obs", "OBS_RELEASE_DSTACK_VERSION")):
        pin = (SCRIPTS / ("%s-release.pin" % comp)).read_text()
        want = [l.split("=", 1)[1] for l in pin.splitlines() if l.startswith(key + "=")][0]
        assert _bash('strih_dante_ptp_pin_dstack "$1"', comp).stdout.strip() == want


# --- the media clock verdict -------------------------------------------------------------------------------

@pytest.mark.parametrize("lines,want", [
    ([INTERNAL], "internal Service -"),
    ([NOT_STARTED, SERVICE], "service None port 319 needs CAP_NET_BIND_SERVICE or root"),
    ([SERVICE, INTERNAL], "internal Service -"),
    ([INTERNAL, NO_SOURCE], "none - -"),
    ([INTERNAL, "x: media clock source source=Internal from=None", SERVICE.replace("12:00:00.500", "12:00:09.000")],
     "service None -"),
    ([], "absent - -"),
    ([NOT_STARTED], "absent - port 319 needs CAP_NET_BIND_SERVICE or root"),
    (["x: media clock source source=Weird"], "odd:Weird - -"),
    # dstack v0.6.31 (live strih-snv hub, 4.11.2026): the switch line carries a re-align phrase, and a
    # "waits to agree" line before it names the CANDIDATE source, which is not yet in use.
    ([SERVICE, "x INFO dstack::clock::linked: media clock: the new source waits to agree with the one in use "
      "source=Internal from=Service offset_ns=Some(-441612)"], "service None -"),
    ([SERVICE, "x INFO dstack::clock::linked: media clock: the new source waits to agree with the one in use "
      "source=Internal from=Service offset_ns=Some(-441612)",
      "x INFO dstack::clock::linked: media clock source changed with a re-align source=Internal from=Service "
      "offset_ns=Some(-455303) waited=60"], "internal Service -"),
    # dstack v0.6.33 (live strih-snv OBS log, 5.11.2026): the seamless switch line.
    ([SERVICE, "04:52:02.662: dante-input dlink:  INFO dstack::clock::linked: media clock source handed over, "
      "no re-align source=Internal from=Service offset_ns=Some(-23454)"], "internal Service -"),
])
def test_the_last_source_line_decides(lines, want):
    r = _bash("strih_dante_media_clock_verdict", stdin="\n".join(lines) + "\n")
    assert r.returncode == 0, r.stderr
    assert r.stdout == want + "\n"


def test_ansi_colour_codes_are_stripped():
    line = "\x1b[2m2026-11-04T10:00:00Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mdstack::clock::linked\x1b[0m: media clock " \
           "source \x1b[3msource\x1b[0m\x1b[2m=\x1b[0mInternal \x1b[3mfrom\x1b[0m\x1b[2m=\x1b[0mService"
    assert _bash("strih_dante_media_clock_verdict", stdin=line + "\n").stdout == "internal Service -\n"


# --- the media clock row -----------------------------------------------------------------------------------

def _clock(tmp, lines, ver="v0.6.23", comp="obs"):
    return _bash('strih_dante_media_clock_grade "(program-dante)" "$1"', comp,
                 env=_pins(tmp, intercom=ver, obs=ver), stdin="\n".join(lines) + "\n")


def test_internal_passes(tmp_path):
    r = _clock(tmp_path, [SERVICE, INTERNAL])
    assert r.stdout.startswith("OK (program-dante) media clock: source=Internal"), r.stdout
    assert "v0.6.23" in r.stdout


@pytest.mark.parametrize("lines,why", [
    ([NOT_STARTED, SERVICE], "source=Service"),
    ([INTERNAL, NO_SOURCE], "no media clock source ready"),
    ([], "no 'media clock source' line"),
    (["x: media clock source source=Weird"], "source=Weird"),
])
def test_anything_but_internal_fails_once_the_pin_is_ready(tmp_path, lines, why):
    r = _clock(tmp_path, lines)
    assert r.returncode == 0 and r.stdout.startswith("BAD (program-dante) media clock: "), r.stdout
    assert why in r.stdout and len(r.stdout.splitlines()) == 1, r.stdout


def test_the_followers_reason_is_named(tmp_path):
    r = _clock(tmp_path, [NOT_STARTED, SERVICE])
    assert "the follower did not start: port 319 needs CAP_NET_BIND_SERVICE or root" in r.stdout, r.stdout


@pytest.mark.parametrize("lines", [[SERVICE], [], [INTERNAL, NO_SOURCE]])
def test_below_the_threshold_it_is_a_note(tmp_path, lines):
    r = _clock(tmp_path, lines, ver="v0.6.19")
    assert r.stdout.startswith("NOTE (program-dante) media clock: "), r.stdout
    assert "below v0.6.23" in r.stdout and "v0.6.19" in r.stdout


def test_the_component_names_its_own_pin(tmp_path):
    env = _pins(tmp_path, intercom="v0.6.19", obs="v0.6.23")
    r = _bash('strih_dante_media_clock_grade "(intercom-dante)" intercom', env=env, stdin=SERVICE + "\n")
    assert r.stdout.startswith("NOTE (intercom-dante)"), r.stdout
    r = _bash('strih_dante_media_clock_grade "(program-dante)" obs', env=env, stdin=SERVICE + "\n")
    assert r.stdout.startswith("BAD (program-dante)"), r.stdout


# --- the OBS's right to bind 319: the sysctl -----------------------------------------------------------------

def _owner():
    u = subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()
    g = subprocess.run(["id", "-gn"], capture_output=True, text=True).stdout.strip()
    return "%s:%s" % (u, g)


def _root(tmp, live="1024", obs="v0.6.23", intercom="v0.6.23"):
    root = tmp / "root"
    proc = root / "proc" / "sys" / "net" / "ipv4" / "ip_unprivileged_port_start"
    proc.parent.mkdir(parents=True)
    proc.write_text(live + "\n")
    return root, proc, root / "etc" / "sysctl.d" / "60-strih-dante-ptp-port.conf", \
        dict(_pins(tmp, obs=obs, intercom=intercom), STRIH_DANTE_RX_PIN_ROOT=str(root), STRIH_DANTE_RX_PIN_OWNER=_owner())


def test_the_dropin_text():
    assert _bash("strih_dante_ptp_port_text").stdout == DROPIN


def test_install_writes_the_dropin_and_sets_the_running_value(tmp_path):
    _, proc, f, env = _root(tmp_path)
    r = _bash(". scripts/lib/obs-box-write.sh; strih_dante_ptp_port_install", env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert f.read_text() == DROPIN and oct(f.stat().st_mode & 0o777) == "0o644"
    assert proc.read_text() == "319\n"
    assert "it binds at OBS's next start" in r.stdout
    r = _bash(". scripts/lib/obs-box-write.sh; strih_dante_ptp_port_install", env=env)
    assert r.returncode == 0 and "unchanged" in r.stdout, r.stdout + r.stderr


def test_below_the_threshold_install_grants_nothing_and_takes_ours_back(tmp_path):
    """Review round 1 (red finding): with the obs pin below v0.6.23 the OBS still runs the old dlink, whose
    follower would start beside dsync once it can bind 319. So step 13 sets nothing and removes ours."""
    _, proc, f, env = _root(tmp_path, live="319", obs="v0.6.19")
    f.parent.mkdir(parents=True)
    f.write_text(DROPIN)
    r = _bash(". scripts/lib/obs-box-write.sh; strih_dante_ptp_port_install", env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert not f.exists() and proc.read_text() == "1024\n", r.stdout
    assert "no 319 bind right yet" in r.stdout and "obs v0.6.19" in r.stdout and "below v0.6.23" in r.stdout
    _, proc2, f2, env2 = _root(tmp_path / "fresh", obs="v0.6.19")
    r = _bash(". scripts/lib/obs-box-write.sh; strih_dante_ptp_port_install", env=env2)
    assert r.returncode == 0 and not f2.exists() and proc2.read_text() == "1024\n", r.stdout + r.stderr


@pytest.mark.parametrize("obs,intercom", [("v0.6.23", "v0.6.19"), ("v0.6.19", "v0.6.23"), ("", "")])
def test_the_bind_right_waits_for_every_dstack_on_the_box(tmp_path, obs, intercom):
    """Review round 2 (red finding): the sysctl is box-wide, so the hub (an unprivileged operator process)
    gets 319 too; an old hub dlink would start its follower beside dsync. Both pins must be READY."""
    _, proc, f, env = _root(tmp_path, live="319", obs=obs, intercom=intercom)
    f.parent.mkdir(parents=True)
    f.write_text(DROPIN)
    r = _bash(". scripts/lib/obs-box-write.sh; strih_dante_ptp_port_install", env=env)
    assert r.returncode == 0 and not f.exists() and proc.read_text() == "1024\n", r.stdout + r.stderr
    assert "no 319 bind right yet" in r.stdout, r.stdout
    r = _bash("strih_dante_ptp_port_grade", env=env)
    assert r.stdout.startswith("NOTE (program-dante) ptp-port: not set, "), r.stdout


def test_install_fails_by_name_when_the_running_value_cannot_be_set(tmp_path):
    _, proc, _, env = _root(tmp_path)
    proc.unlink()
    proc.parent.rmdir()
    r = _bash(". scripts/lib/obs-box-write.sh; strih_dante_ptp_port_install", env=env)
    assert r.returncode == 1 and "cannot set" in r.stderr, r.stdout + r.stderr


def test_another_box_removes_ours_and_puts_the_value_back(tmp_path):
    _, proc, f, env = _root(tmp_path, live="319")
    f.parent.mkdir(parents=True)
    f.write_text(DROPIN)
    r = _bash("strih_dante_ptp_port_remove", env=env)
    assert r.returncode == 0 and not f.exists() and proc.read_text() == "1024\n", r.stdout + r.stderr
    # nothing there: silent, nothing touched
    proc.write_text("500\n")
    r = _bash("strih_dante_ptp_port_remove", env=env)
    assert r.returncode == 0 and r.stdout == "" and proc.read_text() == "500\n"


def test_a_foreign_file_of_the_name_is_left_and_named(tmp_path):
    _, proc, f, env = _root(tmp_path, live="319")
    f.parent.mkdir(parents=True)
    f.write_text("net.ipv4.ip_unprivileged_port_start = 319\n")
    r = _bash("strih_dante_ptp_port_remove", env=env)
    assert r.returncode == 0 and f.exists() and proc.read_text() == "319\n"
    assert "not ours" in r.stderr


def test_removing_ours_leaves_a_value_someone_else_set(tmp_path):
    _, proc, f, env = _root(tmp_path, live="80")
    f.parent.mkdir(parents=True)
    f.write_text(DROPIN)
    r = _bash("strih_dante_ptp_port_remove", env=env)
    assert r.returncode == 0 and not f.exists() and proc.read_text() == "80\n"


def test_the_ptp_port_row_below_the_threshold(tmp_path):
    _, _, f, env = _root(tmp_path, live="1024", obs="v0.6.19")
    r = _bash("strih_dante_ptp_port_grade", env=env)
    assert r.stdout.startswith("NOTE (program-dante) ptp-port: not set, the pins' dstack (intercom v0.6.23, obs v0.6.19)"), r.stdout
    f.parent.mkdir(parents=True)
    f.write_text(DROPIN)
    r = _bash("strih_dante_ptp_port_grade", env=env)
    assert r.stdout.startswith("BAD (program-dante) ptp-port: ") and "is set but" in r.stdout, r.stdout


@pytest.mark.parametrize("dropin,live,want", [
    (DROPIN, "319", "OK (program-dante) ptp-port: unprivileged processes may bind UDP 319"),
    (None, "319", "BAD (program-dante) ptp-port: "),
    (DROPIN.replace("319\n", "318\n"), "319", "BAD (program-dante) ptp-port: "),
    (DROPIN, "1024", "BAD (program-dante) ptp-port: net.ipv4.ip_unprivileged_port_start reads '1024'"),
])
def test_the_ptp_port_row(tmp_path, dropin, live, want):
    _, _, f, env = _root(tmp_path, live=live)
    if dropin is not None:
        f.parent.mkdir(parents=True)
        f.write_text(dropin)
    r = _bash("strih_dante_ptp_port_grade", env=env)
    assert r.returncode == 0 and r.stdout.startswith(want) and len(r.stdout.splitlines()) == 1, r.stdout


# --- verify item 16: the hub's capabilities ------------------------------------------------------------------

def _cap(tmp, ambient, bounding, ver="v0.6.23", filecap=""):
    b = tmp / "bin"
    b.mkdir(exist_ok=True)
    (b / "getcap").write_text("#!/bin/sh\nprintf '%%s' '%s'\n" % filecap)
    (b / "getcap").chmod(0o755)
    env = dict(_pins(tmp, intercom=ver), STRIH_HUB_AMBIENT_CAPS=ambient, STRIH_HUB_BOUNDING_CAPS=bounding,
               PATH="%s:%s" % (b, os.environ["PATH"]))
    return _bash('strih_intercom_hub_cap_grade "$1"\necho END', tmp / "no-such-hub", env=env, lib=HUB_UNIT)


FULL = ("cap_chown cap_dac_override cap_dac_read_search cap_fowner cap_fsetid cap_kill cap_setgid cap_setuid "
        "cap_setpcap cap_linux_immutable cap_net_bind_service cap_net_broadcast cap_net_admin cap_net_raw")


def test_exactly_the_bind_capability_passes(tmp_path):
    r = _cap(tmp_path, "cap_net_bind_service", "cap_net_bind_service")
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("OK (intercom-hub) exactly CAP_NET_BIND_SERVICE"), r.stdout
    assert r.stdout.rstrip().endswith("END")


def test_the_systemctl_spelling_in_upper_case_passes_too(tmp_path):
    r = _cap(tmp_path, "CAP_NET_BIND_SERVICE", " CAP_NET_BIND_SERVICE ")
    assert r.stdout.startswith("OK (intercom-hub)"), r.stdout


@pytest.mark.parametrize("ambient,bounding,filecap,why", [
    ("", "cap_net_bind_service", "", "AmbientCapabilities='none'"),
    ("cap_net_bind_service", FULL, "", "CapabilityBoundingSet="),
    ("cap_net_bind_service", "", "", "CapabilityBoundingSet='none'"),
    ("cap_net_bind_service cap_net_raw", "cap_net_bind_service cap_net_raw", "", "cap_net_raw"),
    ("cap_net_bind_service", "cap_net_bind_service", "/x/intercom-hub cap_net_bind_service=ep", "a file capability"),
    ("cap_net_bind_service", "cap_net_bind_service", "/x/intercom-hub cap_sys_nice=ep", "cap_sys_nice"),
])
def test_missing_or_another_capability_fails(tmp_path, ambient, bounding, filecap, why):
    r = _cap(tmp_path, ambient, bounding, filecap=filecap)
    assert r.returncode == 0, r.stderr
    row = r.stdout.splitlines()[0]
    assert row.startswith("BAD (intercom-hub) capabilities: ") and why in row, r.stdout
    assert r.stdout.rstrip().endswith("END")


@pytest.mark.parametrize("ambient,bounding", [("", ""), ("cap_net_bind_service", "cap_net_bind_service")])
def test_below_the_threshold_the_capability_row_is_a_note(tmp_path, ambient, bounding):
    r = _cap(tmp_path, ambient, bounding, ver="v0.6.19")
    assert r.stdout.startswith("NOTE (intercom-hub) capabilities not graded:"), r.stdout
    assert "v0.6.19" in r.stdout and "below v0.6.23" in r.stdout


@pytest.mark.parametrize("render", [
    'strih_intercom_audio_dropin newlevel 1000 pipewire',
    'strih_intercom_audio_dropin newlevel 1000 no-pipewire',
    'strih_intercom_hub_cred_dropin_text',
    'ndi_discovery_dropin_content',
    'strih_public_dropin_text',
])
def test_no_hub_dropin_touches_the_released_units_capabilities(render):
    """The intercom project (4.11.2026): the released unit's AmbientCapabilities= + CapabilityBoundingSet= are
    the only grant; a drop-in setting either (an empty one resets the list) silently takes UDP 319 away."""
    r = _bash('. scripts/lib/ndi-discovery.sh; . scripts/lib/strih-interkom-public.sh; %s' % render, lib=PROVISION)
    assert r.returncode == 0 and r.stdout.strip(), (render, r.stderr)
    for key in ("AmbientCapabilities", "CapabilityBoundingSet", "SecureBits"):
        assert key not in r.stdout, (render, key)


def test_step_13_writes_no_other_hub_dropin():
    """Every drop-in step 13 puts beside the released unit is one of the renderers above."""
    s = (SCRIPTS / "setup-strih.sh").read_text()
    step = _between(s, 'step 13 "Intercom hub unit', "\nfi\n")
    writes = [l for l in step.splitlines() if "intercom-hub.service.d" in l and ">" in l]
    assert writes == ['  strih_intercom_audio_dropin "$DESKTOP_USER" "$DESKTOP_UID" "$(strih_pipewire_purge_dropin_mode)" '
                      '> /etc/systemd/system/intercom-hub.service.d/10-local-audio.conf   # issue 1440: no pipewire '
                      'group on a purged box'], writes


# --- verify item 16: the hub's media clock -------------------------------------------------------------------

def _hub_clock(tmp, box, lines):
    j = tmp / "journal.txt"
    j.write_text("\n".join(lines) + "\n")
    env = dict(_pins(tmp), STRIH_HUB_JOURNAL_FILE=str(j))
    return _bash('strih_box_load "$1" >/dev/null\nstrih_intercom_hub_media_clock_grade\necho END', box, env=env,
                 lib=PROVISION)


def test_the_hub_media_clock_is_graded_on_a_box_with_the_hub_device(tmp_path):
    line = "INFO dstack::clock::linked: media clock source source=Internal from=Service"
    r = _hub_clock(tmp_path, "strih-snv", [line])
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("OK (intercom-dante) media clock: source=Internal"), r.stdout
    r = _hub_clock(tmp_path, "strih-snv", [line.replace("Internal", "Service")])
    assert r.stdout.startswith("BAD (intercom-dante) media clock: source=Service"), r.stdout


def test_a_hub_that_is_not_running_is_a_note_not_a_second_fail(tmp_path):
    env = dict(_pins(tmp_path), STRIH_HUB_INVOCATION_ID="")
    r = _bash('strih_box_load strih-snv >/dev/null\nstrih_intercom_hub_media_clock_grade', env=env, lib=PROVISION)
    assert r.returncode == 0 and r.stdout.startswith("NOTE (intercom-dante) media clock: the hub is not running"), r.stdout


def test_an_unreadable_journal_is_named_never_read_as_no_line(tmp_path):
    """Review round 1: a journalctl that times out or is refused must not read as 'no media clock line'."""
    b = tmp_path / "bin"
    b.mkdir()
    (b / "journalctl").write_text("#!/bin/sh\necho 'Failed to open journal' >&2\nexit 1\n")
    (b / "journalctl").chmod(0o755)
    env = dict(_pins(tmp_path), STRIH_HUB_INVOCATION_ID="abc123", STRIH_HUB_JOURNAL_EUID="0",
               PATH="%s:%s" % (b, os.environ["PATH"]))
    r = _bash('strih_box_load strih-snv >/dev/null\nstrih_intercom_hub_media_clock_grade', env=env, lib=PROVISION)
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("BAD (intercom-dante) media clock: the hub's journal could not be read (rc 1"), r.stdout
    assert "no 'media clock source' line" not in r.stdout


def test_a_seam_file_without_a_trailing_newline_is_still_read(tmp_path):
    j = tmp_path / "journal.txt"
    j.write_text("INFO dstack::clock::linked: media clock source source=Internal from=Service")
    env = dict(_pins(tmp_path), STRIH_HUB_JOURNAL_FILE=str(j))
    r = _bash('strih_box_load strih-snv >/dev/null\nstrih_intercom_hub_media_clock_grade', env=env, lib=PROVISION)
    assert r.stdout.startswith("OK (intercom-dante) media clock: source=Internal"), r.stdout + r.stderr


def test_a_journal_read_as_non_root_says_so(tmp_path):
    """journalctl exits 0 with only a permission hint for a user who may not read the system journal."""
    b = tmp_path / "bin"
    b.mkdir()
    (b / "journalctl").write_text("#!/bin/sh\necho 'Hint: You are currently not seeing messages from other users' >&2\n")
    (b / "journalctl").chmod(0o755)
    env = dict(_pins(tmp_path), STRIH_HUB_INVOCATION_ID="abc123", STRIH_HUB_JOURNAL_EUID="1000",
               STRIH_HUB_JOURNAL_GROUPS="newlevel sudo", PATH="%s:%s" % (b, os.environ["PATH"]))
    r = _bash('strih_box_load strih-snv >/dev/null\nstrih_intercom_hub_media_clock_grade', env=env, lib=PROVISION)
    assert r.stdout.startswith("BAD (intercom-dante) media clock: "), r.stdout
    assert "run verify-strih as root" in r.stdout, r.stdout


def test_a_non_root_journal_read_below_the_threshold_is_a_note(tmp_path):
    """Review round 3: today's pins are below v0.6.23 and verify-strih supports a non-root run."""
    env = dict(_pins(tmp_path, intercom="v0.6.19"), STRIH_HUB_INVOCATION_ID="abc123", STRIH_HUB_JOURNAL_EUID="1000",
               STRIH_HUB_JOURNAL_GROUPS="newlevel sudo")
    r = _bash('strih_box_load strih-snv >/dev/null\nstrih_intercom_hub_media_clock_grade', env=env, lib=PROVISION)
    assert r.stdout.startswith("NOTE (intercom-dante) media clock: "), r.stdout
    assert "not readable as" in r.stdout and "below v0.6.23" in r.stdout, r.stdout


@pytest.mark.parametrize("group", ["systemd-journal", "adm"])
def test_a_journal_group_member_reads_the_journal(tmp_path, group):
    b = tmp_path / "bin"
    b.mkdir()
    (b / "journalctl").write_text('#!/bin/sh\nprintf "INFO dstack::clock::linked: media clock source source=Internal from=None\\n"\n')
    (b / "journalctl").chmod(0o755)
    env = dict(_pins(tmp_path), STRIH_HUB_INVOCATION_ID="abc123", STRIH_HUB_JOURNAL_EUID="1000",
               STRIH_HUB_JOURNAL_GROUPS="newlevel %s sudo" % group, PATH="%s:%s" % (b, os.environ["PATH"]))
    r = _bash('strih_box_load strih-snv >/dev/null\nstrih_intercom_hub_media_clock_grade', env=env, lib=PROVISION)
    assert r.stdout.startswith("OK (intercom-dante) media clock: source=Internal"), r.stdout + r.stderr


def test_the_journal_is_read_for_the_current_invocation_only(tmp_path):
    b = tmp_path / "bin"
    b.mkdir()
    log = tmp_path / "args"
    (b / "journalctl").write_text('#!/bin/sh\nprintf "%%s\\n" "$@" > "%s"\n'
                                  'printf "noise\\nINFO dstack::clock::linked: media clock source source=Internal from=None\\n"\n' % log)
    (b / "journalctl").chmod(0o755)
    env = dict(_pins(tmp_path), STRIH_HUB_INVOCATION_ID="abc123", STRIH_HUB_JOURNAL_EUID="0",
               PATH="%s:%s" % (b, os.environ["PATH"]))
    r = _bash('strih_box_load strih-snv >/dev/null\nstrih_intercom_hub_media_clock_grade', env=env, lib=PROVISION)
    assert r.stdout.startswith("OK (intercom-dante) media clock: source=Internal"), r.stdout + r.stderr
    assert "_SYSTEMD_INVOCATION_ID=abc123" in log.read_text().splitlines()


def test_a_box_without_the_hub_device_prints_nothing(tmp_path):
    r = _hub_clock(tmp_path, "strih-pp", [])
    assert r.returncode == 0 and r.stdout == "END\n", r.stdout + r.stderr


# --- the wiring ----------------------------------------------------------------------------------------------

def _between(text, a, b):
    i = text.index(a)
    return text[i:text.index(b, i)]


def test_step_13_installs_on_a_dante_box_and_removes_elsewhere():
    s = (SCRIPTS / "lib" / "strih-dante-input.sh").read_text()
    inst = _between(s, "strih_dante_input_install() {", "\n}\n")
    other = _between(inst, "if ! strih_program_dante_declared; then", "    return 0\n  fi\n")
    assert "strih_dante_ptp_port_remove || return 1" in other
    assert "strih_dante_ptp_port_install ||" in inst.split(other, 1)[1]


def test_item_9_grades_the_media_clock_and_the_port():
    s = (SCRIPTS / "lib" / "strih-dante-input.sh").read_text()
    g = _between(s, "strih_dante_input_grade() {", "\n}\n")
    assert 'strih_dante_media_clock_grade "(program-dante)" obs < "$log"' in g
    # the clock row after the readable-log check (the early return); the port row reads no log, before it
    assert g.index("no readable OBS log") < g.index("strih_dante_media_clock_grade")
    assert g.index("strih_dante_ptp_port_grade") < g.index("no readable OBS log")


def test_item_16_grades_the_hub_media_clock():
    v = (SCRIPTS / "verify-strih.sh").read_text()
    item = _between(v, "# 16) intercom hub unit", "# 17) ")
    assert item.count("strih_intercom_hub_media_clock_grade") == 1
    assert item.index("strih_intercom_hub_version_grade") < item.index("strih_intercom_hub_media_clock_grade")


def test_no_lib_still_calls_dsync_the_only_ptp_owner():
    for f in ("strih-dante-leg.sh", "strih-intercom-hub-unit.sh", "strih-dante-input.sh"):
        assert "only PTP owner" not in (SCRIPTS / "lib" / f).read_text(), f
