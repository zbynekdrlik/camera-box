"""Owner 20.11.2026 (via dstack#465): "a povazujem to za vazny fail ze ktory kolvek z nasich zariadeni co su v
rigu ma eee povolene!!" -- EEE enabled on ANY wired NIC of a rig box is a serious failure.

dstack's read-only scan found EEE `enabled - inactive` on four unused onboard NICs of our Linux OBS boxes
(stream-snv enp56s0, strih-snv enp2s0, strih-pp enp6s0 + enp7s0): the baseline turned EEE off on the rig
NIC only. Now:
  * `obs_box_eee_off_cmds` = bash that turns EEE off on EVERY wired NIC (a /sys/class/net entry with a
    device link that is not wireless), up or down; the rc.local boot hook, the networkd-dispatcher hook and
    the immediate apply all run it;
  * the gather reads `ethtool --show-eee` of every wired NIC: `eee_on=` names the ones that read enabled,
    `eee_unread=` the ones whose state could not be read; the verdict row `eee` FAILs on either.
"""
import os
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
BASELINE = REPO / "scripts" / "lib" / "obs-box-baseline.sh"
VERIFY = REPO / "scripts" / "lib" / "obs-box-baseline-verify.sh"


def _sysfs(root, nics):
    """nics: name -> kind ('wired' | 'wireless' | 'virtual')."""
    net = root / "class" / "net"
    net.mkdir(parents=True)
    for name, kind in nics.items():
        d = net / name
        d.mkdir()
        if kind != "virtual":
            (d / "device").mkdir()
        if kind == "wireless":
            (d / "wireless").mkdir()
    return root


def _ethtool(tmp_path, states):
    """A fake ethtool: --set-eee is logged; --show-eee prints the state of `states[nic]` (missing = error)."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    log = tmp_path / "ethtool.log"
    lines = "\n".join(f'  {n}) echo "EEE settings for {n}:"; echo "        EEE status: {s}";;' for n, s in states.items())
    (bindir / "ethtool").write_text(
        "#!/bin/bash\n"
        f'echo "$*" >> "{log}"\n'
        'if [ "$1" = --show-eee ]; then case "$2" in\n'
        f"{lines}\n"
        '  *) echo "Cannot get EEE settings: No such device" >&2; exit 75;;\n'
        "esac; fi\n"
        "exit 0\n")
    (bindir / "ethtool").chmod(0o755)
    return bindir, log


def _bash(script, *args, env=None):
    e = dict(os.environ)
    e.update(env or {})
    return subprocess.run(["bash", "-c", script, "harness", *args], capture_output=True, text=True, env=e)


def test_eee_goes_off_on_every_wired_nic_up_or_down_never_on_wireless_or_virtual(tmp_path):
    sysroot = _sysfs(tmp_path / "sys", {"enp56s0": "wired", "rig0": "wired", "wlo1": "wireless",
                                       "lo": "virtual", "br0": "virtual"})
    bindir, log = _ethtool(tmp_path, {})
    r = _bash('set -euo pipefail; . "$1"; script="$(obs_box_eee_off_cmds "$2")"; bash -c "$script"',
              str(BASELINE), str(sysroot), env={"PATH": f"{bindir}:{os.environ['PATH']}"})
    assert r.returncode == 0, r.stderr
    calls = log.read_text().splitlines()
    assert sorted(calls) == ["--set-eee enp56s0 eee off", "--set-eee rig0 eee off"]


def test_a_nic_whose_eee_cannot_be_set_never_stops_the_others(tmp_path):
    sysroot = _sysfs(tmp_path / "sys", {"enx1": "wired", "enp2s0": "wired"})
    bindir, log = _ethtool(tmp_path, {})
    (bindir / "ethtool").write_text(f'#!/bin/bash\necho "$*" >> "{log}"\n[ "$2" = enx1 ] && exit 75\nexit 0\n')
    r = _bash('set -euo pipefail; . "$1"; script="$(obs_box_eee_off_cmds "$2")"; bash -c "set -euo pipefail; $script"',
              str(BASELINE), str(sysroot), env={"PATH": f"{bindir}:{os.environ['PATH']}"})
    assert r.returncode == 0, r.stderr
    assert "--set-eee enp2s0 eee off" in log.read_text()


def test_every_eee_writer_runs_the_all_nics_command():
    s = BASELINE.read_text()
    body = s[s.index("obs_box_network_tuning() {"):s.index("obs_box_eee_off_cmds() {")]
    assert "obs_box_eee_off_cmds" in body, "the dispatcher hook + the immediate apply cover every wired NIC"
    perf = s[s.index("obs_box_max_performance() {"):]
    perf = perf[:perf.index("\n}\n")]
    assert "obs_box_eee_off_cmds" in perf, "the rc.local boot hook covers every wired NIC"
    assert 'ethtool --set-eee "${NIC}" eee off' not in body and "ethtool --set-eee ${NIC} eee off" not in perf


def _gather_eee(tmp_path, nics, states):
    sysroot = _sysfs(tmp_path / "sys", nics)
    bindir, _ = _ethtool(tmp_path, states)
    r = _bash('. "$1"; obs_box_eee_state "$2"', str(VERIFY), str(sysroot),
              env={"PATH": f"{bindir}:{os.environ['PATH']}"})
    assert r.returncode == 0, r.stderr
    return dict(ln.split("=", 1) for ln in r.stdout.splitlines() if "=" in ln)


def test_the_gather_names_every_wired_nic_with_eee_enabled_even_inactive(tmp_path):
    f = _gather_eee(tmp_path, {"enp56s0": "wired", "rig0": "wired", "wlo1": "wireless"},
                    {"enp56s0": "enabled - inactive", "rig0": "disabled", "wlo1": "enabled - active"})
    assert f == {"eee_on": "enp56s0", "eee_unread": ""}


def test_a_nic_without_eee_support_is_fine_and_an_unreadable_one_is_named(tmp_path):
    f = _gather_eee(tmp_path, {"enx1": "wired", "enp2s0": "wired"},
                    {"enx1": "not supported"})
    assert f == {"eee_on": "", "eee_unread": "enp2s0"}


# --- dstack#465 follow-up (20.11.2026): r8152 forgets `--set-eee` on every USB re-attach, and the
# networkd-dispatcher hook never fires for a NetworkManager-managed NIC (stream-snv rig0 read
# "enabled - inactive" again after an unbind/bind). A udev `add` rule turns EEE off on every wired NIC
# the moment the device appears, before its link comes up (a set on an up link renegotiates it).
UDEV = "/etc/udev/rules.d/70-obs-box-eee-off.rules"


def test_the_udev_rule_turns_eee_off_on_every_wired_nic_at_add():
    r = _bash('. "$1"; obs_box_eee_udev_rule', str(BASELINE))
    assert r.returncode == 0, r.stderr
    rule = [ln for ln in r.stdout.splitlines() if ln and not ln.startswith("#")]
    assert len(rule) == 1, r.stdout
    for part in ('ACTION=="add"', 'SUBSYSTEM=="net"', 'TEST=="device"', 'TEST!="wireless"', 'TEST!="phy80211"',
                 'RUN+="/usr/sbin/ethtool --set-eee $name eee off"'):
        assert part in rule[0], (part, rule[0])


def test_the_udev_install_writes_the_rule_and_reloads_only_on_a_change(tmp_path):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    log = tmp_path / "udevadm.log"
    (bindir / "udevadm").write_text('#!/bin/bash\necho "$*" >> "%s"\n' % log)
    (bindir / "udevadm").chmod(0o755)
    import pwd
    import grp
    owner = "%s:%s" % (pwd.getpwuid(os.getuid()).pw_name, grp.getgrgid(os.getgid()).gr_name)
    dest = tmp_path / "70-obs-box-eee-off.rules"
    env = {"PATH": "%s:%s" % (bindir, os.environ["PATH"]), "OBS_BOX_EEE_UDEV_RULE": str(dest),
           "OBS_BOX_EEE_UDEV_OWNER": owner}
    script = 'set -euo pipefail; fail() { echo "FAIL $*" >&2; return 1; }; . "$1"; obs_box_eee_udev_install'
    r = _bash(script, str(BASELINE), env=env)
    assert r.returncode == 0, r.stderr
    want = _bash('. "$1"; obs_box_eee_udev_rule', str(BASELINE)).stdout
    assert dest.read_text() == want
    assert "control --reload-rules" in log.read_text()
    log.unlink()
    r = _bash(script, str(BASELINE), env=env)
    assert r.returncode == 0, r.stderr
    assert not log.exists(), "an unchanged rule never reloads udev"


def test_the_network_tuning_installs_the_udev_rule():
    s = BASELINE.read_text()
    body = s[s.index("obs_box_network_tuning() {"):s.index("obs_box_eee_off_cmds() {")]
    assert "obs_box_eee_udev_install" in body


def test_the_gather_grades_the_udev_rule_against_its_renderer():
    s = VERIFY.read_text()
    assert "obs_box_eee_udev_rule" in s and "eee_udev=" in s
