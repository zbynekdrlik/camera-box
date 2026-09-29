"""Issue 1391 -- the strih rig NIC's out-of-tree driver (the Realtek RTL8157 5 GbE adapter) provisioned
through DKMS from a vendored source, and verify-strih item 36 grading the facts that decide whether the
5 GbE link loses packets.

WHY: on 29.9.2026 strih-lx got a Ubiquiti UACC-Adapter-RJ45-USBC-5GE (Realtek RTL8157, USB 0bda:8157).
The in-tree r8152 of the 7.0 kernels does not bind it, so the Realtek driver v2.21.4 was built on dev1 and
copied by hand as a plain module to /lib/modules/<kernel>/updates/r8152.ko -- the next kernel boots
without it. Measured live the same day: the laptop's USB-C port trained the adapter at Gen 1 (5000 Mb/s)
one way round, and at the 5 GbE link it lost ~240 packets/s; flipped 180 degrees it trained at 10000 Mb/s
and lost none (main design 5889434704, Approach 1).

What this pins:
  * the vendored tree vendor/realtek-r8152: SHA256SUMS verifies, no dev1 build artifact, dkms.conf names
    match the lib's constants, the source is the RTL8157-capable v2.21.4;
  * the facts loader: the three new facts (STRIH_NIC_OOT_DRIVER, STRIH_NIC_MIN_USB_MBPS,
    STRIH_NIC_MIN_LINK_MBPS) load for strih-lx, are TODO_OWNER in strih-pp, refuse a missing / malformed
    value;
  * the PURE planner scripts/lib/strih-nic-driver.sh `strih_nic_driver_plan` (SKIP/NOOP/INSTALL/UPGRADE)
    over `dkms status` fixtures in both DKMS shapes;
  * the setup-strih step 1b apply `strih_nic_driver_apply`, run through its path seams with a fake
    `dkms` state machine, `modinfo`, `depmod`, `udevadm`, `apt-get` and a forbidden `modprobe`/`rmmod`
    on PATH: the live 29.9 shape (a hand-copied plain module) is replaced by DKMS in the safe order
    (build, move the plain copy aside, install, restore it if the install fails), a second run is a NOOP,
    the module is never reloaded live;
  * verify-strih item 36 `strih_nic_grade_rows` over a fake sysfs tree + a fake `nmcli`/`dkms`: PASS at
    10000/5000, FAIL at a Gen 1 USB link naming the connector-flip fix, FAIL at a 2500 link, an unreadable
    speed never passes;
  * the wiring: setup-strih step 1b, verify-strih item 36, the deploy archive staging vendor/realtek-r8152.

Tier-0: bash + pytest only (no cargo, no root, no rig).
"""
import hashlib
import os
import re
import stat
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "strih-nic-driver.sh"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
DEPLOY = SCRIPTS / "lib" / "strih-lx-deploy.sh"
VENDOR = REPO / "vendor" / "realtek-r8152"
BOXES = SCRIPTS / "strih-boxes"

KERNEL = "7.0.0-31-generic"
SPEC = "realtek-r8152-2.21.4"
NIC = "enx002427159965"
LOADED = "v2.21.4 (2025/10/28)"

# env that could leak box facts / seams in from the caller's shell
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_LX_DANTESYNC_ROLE", "STRIH_LX_NTP_SERVER", "STRIH_NIC_IFACE",
         "STRIH_BOXES_DIR", "OBS_FLEET", "STRIH_NIC_DRV_KERNEL", "STRIH_NIC_DRV_MODULES_ROOT",
         "STRIH_NIC_DRV_SRC_ROOT", "STRIH_NIC_DRV_UDEV_DIR", "STRIH_NIC_DRV_BACKUP_DIR", "STRIH_NIC_DRV_SYSROOT")


def _bash(body, env=None, sources=(LIB,)):
    """Source `sources` in a fresh bash, then run `body`. Returns the CompletedProcess."""
    harness = "set -uo pipefail\n" + "".join('. "%s"\n' % s for s in sources) + body
    full_env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    if env:
        full_env.update(env)
    return subprocess.run(["bash", "-c", harness], capture_output=True, text=True, env=full_env,
                          cwd=str(REPO), timeout=60)


def _code_lines(path):
    return [ln for ln in path.read_text().splitlines() if ln.strip() and not ln.lstrip().startswith("#")]


# --- the vendored tree -----------------------------------------------------------------------------

VENDORED_FILES = {"r8152.c", "compatibility.h", "Makefile", "50-usb-realtek-net.rules", "LICENSE",
                  "ReadMe.txt", "dkms.conf"}


def _sums():
    rows = {}
    for line in (VENDOR / "SHA256SUMS").read_text().splitlines():
        digest, name = line.split(None, 1)
        rows[name.strip()] = digest
    return rows


def test_vendored_tree_is_exactly_the_source_files_plus_the_manifest():
    names = {p.name for p in VENDOR.iterdir()}
    assert names == VENDORED_FILES | {"SHA256SUMS"}, names
    for p in VENDOR.iterdir():
        assert p.is_file() and not p.is_symlink(), p


def test_vendored_tree_carries_no_build_artifact():
    artifact = re.compile(r"(\.o|\.ko|\.mod|\.mod\.c|\.cmd|\.a)$|^Module\.symvers$|^modules\.order$|^\.")
    for p in VENDOR.iterdir():
        assert not artifact.search(p.name), "dev1 build artifact vendored: %s" % p.name


def test_sha256sums_lists_every_vendored_file_and_verifies():
    sums = _sums()
    assert set(sums) == VENDORED_FILES, sorted(sums)
    for name, digest in sums.items():
        assert hashlib.sha256((VENDOR / name).read_bytes()).hexdigest() == digest, name


def test_sha256sums_verifies_with_sha256sum_the_way_the_lib_checks_it():
    r = subprocess.run(["sha256sum", "--quiet", "--strict", "-c", "SHA256SUMS"], cwd=str(VENDOR),
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stdout + r.stderr


def _lib_constants():
    r = _bash('printf "%s\\n" "$STRIH_NIC_DRV_PACKAGE" "$STRIH_NIC_DRV_VERSION" "$STRIH_NIC_DRV_MODULE" '
              '"$STRIH_NIC_DRV_VENDOR_DIR" "$STRIH_NIC_DRV_UDEV_RULE" "$(strih_nic_driver_vendored_spec)"')
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


def test_dkms_conf_names_match_the_lib_constants():
    pkg, ver, mod, vdir, rule, spec = _lib_constants()
    conf = {}
    for line in (VENDOR / "dkms.conf").read_text().splitlines():
        m = re.match(r'^([A-Z_]+(?:\[0\])?)="([^"]*)"$', line)
        if m:
            conf[m.group(1)] = m.group(2)
    assert conf == {"PACKAGE_NAME": pkg, "PACKAGE_VERSION": ver, "BUILT_MODULE_NAME[0]": mod,
                    "DEST_MODULE_LOCATION[0]": "/updates/dkms", "AUTOINSTALL": "yes"}, conf
    assert (REPO / vdir) == VENDOR
    assert (VENDOR / rule).is_file()
    assert spec == SPEC


def test_vendored_source_is_the_rtl8157_capable_release():
    src = (VENDOR / "r8152.c").read_text()
    assert '#define DRIVER_VERSION "v2.21.4"' in src
    assert "REALTEK_USB_DEVICE(VENDOR_ID_REALTEK, 0x8157)" in src
    rule = (VENDOR / "50-usb-realtek-net.rules").read_text()
    # the vendor configuration is selected for 0bda:8157, so r8152 binds instead of cdc_ncm
    assert re.search(r'ATTR\{idVendor\}=="0bda", ATTR\{idProduct\}=="815\[[^]]*7[^]]*\]"', rule), rule


# --- the facts loader ------------------------------------------------------------------------------

NEW_KEYS = ("STRIH_NIC_OOT_DRIVER", "STRIH_NIC_MIN_USB_MBPS", "STRIH_NIC_MIN_LINK_MBPS")


def test_loader_lists_the_new_keys_after_the_nic_driver_key():
    r = _bash("strih_box_fact_keys", sources=(FACTS,))
    keys = r.stdout.split()
    i = keys.index("STRIH_NIC_DRIVER")
    assert tuple(keys[i + 1:i + 4]) == NEW_KEYS, keys


def test_strih_lx_loads_the_new_facts():
    r = _bash("strih_box_load strih-lx || exit 9\n"
              'printf "%s|%s|%s\\n" "$(strih_lx_nic_oot_driver)" "$(strih_lx_nic_min_usb_mbps)" '
              '"$(strih_lx_nic_min_link_mbps)"', sources=(FACTS,))
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "%s|10000|5000" % SPEC


def test_strih_pp_names_every_new_fact_as_todo_owner():
    r = _bash("strih_box_load strih-pp", sources=(FACTS,))
    assert r.returncode != 0
    for k in NEW_KEYS:
        assert "%s is TODO_OWNER" % k in r.stderr, r.stderr


def _fixture(tmp_path, edits):
    body = (BOXES / "strih-lx.env").read_text()
    for old, new in edits:
        assert body.count(old) == 1, old
        body = body.replace(old, new)
    (tmp_path / "strih-lx.env").write_text(body)
    return {"STRIH_BOXES_DIR": str(tmp_path)}


@pytest.mark.parametrize("key", NEW_KEYS)
def test_a_missing_new_fact_refuses(tmp_path, key):
    line = next(ln for ln in (BOXES / "strih-lx.env").read_text().splitlines() if ln.startswith(key + "="))
    env = _fixture(tmp_path, [(line + "\n", "")])
    r = _bash("strih_box_load strih-lx", env=env, sources=(FACTS,))
    assert r.returncode != 0
    assert "missing fact %s" % key in r.stderr, r.stderr


@pytest.mark.parametrize("key,value", [
    ("STRIH_NIC_OOT_DRIVER", "realtek"),
    ("STRIH_NIC_OOT_DRIVER", "-r8152-2.21.4"),
    ("STRIH_NIC_OOT_DRIVER", "realtek-r8152-2"),
    ("STRIH_NIC_OOT_DRIVER", "Realtek-r8152-2.21.4"),
    ("STRIH_NIC_MIN_USB_MBPS", "0"),
    ("STRIH_NIC_MIN_USB_MBPS", "fast"),
    ("STRIH_NIC_MIN_USB_MBPS", "010000"),
    ("STRIH_NIC_MIN_LINK_MBPS", "none"),
    ("STRIH_NIC_MIN_LINK_MBPS", "5 000"),
])
def test_a_malformed_new_fact_refuses_by_name(tmp_path, key, value):
    line = next(ln for ln in (BOXES / "strih-lx.env").read_text().splitlines() if ln.startswith(key + "="))
    env = _fixture(tmp_path, [(line, "%s=%s" % (key, value))])
    r = _bash("strih_box_load strih-lx", env=env, sources=(FACTS,))
    assert r.returncode != 0, value
    assert "%s '%s'" % (key, value) in r.stderr, r.stderr


def test_none_is_accepted_for_the_driver_and_the_usb_speed(tmp_path):
    env = _fixture(tmp_path, [("STRIH_NIC_OOT_DRIVER=%s" % SPEC, "STRIH_NIC_OOT_DRIVER=none"),
                              ("STRIH_NIC_MIN_USB_MBPS=10000", "STRIH_NIC_MIN_USB_MBPS=none")])
    r = _bash("strih_box_load strih-lx && strih_lx_nic_oot_driver && echo && strih_lx_nic_min_usb_mbps",
              env=env, sources=(FACTS,))
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["none", "none"]


# --- the pure planner ------------------------------------------------------------------------------

DKMS3_INSTALLED = "realtek-r8152/2.21.4, %s, x86_64: installed\n" % KERNEL
DKMS3_WARN = "realtek-r8152/2.21.4, %s, x86_64: installed (WARNING! Diff between built and installed module!)\n" % KERNEL
DKMS2_INSTALLED = "realtek-r8152, 2.21.4, %s, x86_64: installed\n" % KERNEL
NVIDIA = "nvidia/595.45, %s, x86_64: installed\n" % KERNEL


def _rows(status):
    r = _bash('strih_nic_dkms_rows realtek-r8152 <<<"$S"', env={"S": status})
    assert r.returncode == 0, r.stderr
    return r.stdout.splitlines()


def test_dkms_rows_read_both_dkms_shapes_and_skip_other_packages():
    assert _rows(NVIDIA + DKMS3_INSTALLED) == ["2.21.4|%s|installed" % KERNEL]
    assert _rows(DKMS2_INSTALLED) == ["2.21.4|%s|installed" % KERNEL]
    assert _rows(DKMS3_WARN) == ["2.21.4|%s|installed" % KERNEL]
    assert _rows("realtek-r8152/2.21.4: added\n") == ["2.21.4||added"]
    assert _rows("realtek-r8152, 2.21.3: added\n") == ["2.21.3||added"]
    assert _rows("realtek-r8152/2.21.4: broken\n") == ["2.21.4||broken"]
    assert _rows("realtek-r8152-extra/1.0: added\n") == []
    assert _rows("") == []


@pytest.mark.parametrize("status,want", [
    ("", "absent"),
    (NVIDIA, "absent"),
    ("realtek-r8152/2.21.4: added\n", "added"),
    ("realtek-r8152/2.21.4, 6.8.0-1-generic, x86_64: installed\n", "added"),
    ("realtek-r8152/2.21.4, %s, x86_64: built\n" % KERNEL, "built"),
    (DKMS3_INSTALLED, "installed"),
    (DKMS3_WARN, "installed"),
    (DKMS2_INSTALLED, "installed"),
    ("realtek-r8152/2.21.4, %s, x86_64: installed-weak from 6.8.0-1-generic\n" % KERNEL, "added"),
    ("realtek-r8152/2.21.4: broken\n", "broken"),
    ("realtek-r8152/2.21.3, %s, x86_64: installed\n" % KERNEL, "absent"),
])
def test_dkms_state(status, want):
    r = _bash('strih_nic_dkms_state realtek-r8152 2.21.4 "$K" <<<"$S"', env={"S": status, "K": KERNEL})
    assert r.returncode == 0, r.stderr
    assert r.stdout == want


@pytest.mark.parametrize("text,want", [
    ("v2.21.4 (2025/10/28)", "2.21.4"), ("2.21.4", "2.21.4"), ("  v2.21.4\n", "2.21.4"),
    ("v1.12.13", "1.12.13"), ("", None), ("v2", None), ("unknown", None), ("vx.y", None),
])
def test_version_norm(text, want):
    r = _bash('strih_nic_driver_version_norm "$T"', env={"T": text})
    if want is None:
        assert r.returncode != 0 and r.stdout == "", (text, r.stdout)
    else:
        assert r.returncode == 0 and r.stdout == want, (text, r.stdout)


def _plan(status, spec=SPEC, kernel=KERNEL, modinfo=LOADED, plain=""):
    return _bash('strih_nic_driver_plan "$SP" "$K" "$MI" "$PL" <<<"$S"',
                 env={"S": status, "SP": spec, "K": kernel, "MI": modinfo, "PL": plain})


@pytest.mark.parametrize("status,modinfo,plain,want", [
    # the target state: DKMS installed for the running kernel, nothing else left
    (DKMS3_INSTALLED, LOADED, "", "NOOP"),
    (NVIDIA + DKMS2_INSTALLED, LOADED, "", "NOOP"),
    # the live 29.9 shape: only the hand-copied plain module
    ("", LOADED, "2.21.4", "UPGRADE"),
    (NVIDIA, LOADED, "unknown", "UPGRADE"),
    # DKMS in place but the plain copy is still there
    (DKMS3_INSTALLED, LOADED, "2.21.4", "UPGRADE"),
    # another DKMS version of the package is the one installed for the running kernel
    ("realtek-r8152/2.21.3, %s, x86_64: installed\n" % KERNEL, "v2.21.3 (2025/01/01)", "", "UPGRADE"),
    # ... but one left on ANOTHER kernel (no headers there) or only added does not change this plan
    (DKMS3_INSTALLED + "realtek-r8152/2.21.3, 7.0.0-28-generic, x86_64: installed\n", LOADED, "", "NOOP"),
    (DKMS3_INSTALLED + "realtek-r8152/2.21.3: added\n", LOADED, "", "NOOP"),
    ("realtek-r8152/2.21.3, 7.0.0-28-generic, x86_64: installed\n", "", "", "INSTALL"),
    # a fresh box: the in-tree driver has no version
    ("", "", "", "INSTALL"),
    ("realtek-r8152/2.21.4: added\n", "", "", "INSTALL"),
    ("realtek-r8152/2.21.4, %s, x86_64: built\n" % KERNEL, "", "", "INSTALL"),
    # DKMS says installed, but the module the kernel would load is another one (no depmod yet)
    (DKMS3_INSTALLED, "", "", "INSTALL"),
    ("realtek-r8152/2.21.4: broken\n", "", "", "INSTALL"),
])
def test_plan_table(status, modinfo, plain, want):
    r = _plan(status, modinfo=modinfo, plain=plain)
    assert r.returncode == 0, r.stderr
    assert r.stdout == want


def test_plan_skips_a_box_with_no_out_of_tree_driver():
    r = _plan(DKMS3_INSTALLED, spec="none")
    assert r.returncode == 0 and r.stdout == "SKIP"


@pytest.mark.parametrize("spec,kernel", [("realtek-r8152-2.21.3", KERNEL), ("other-1.0", KERNEL), ("", KERNEL),
                                         (SPEC, "")])
def test_plan_refuses_a_foreign_spec_or_no_kernel(spec, kernel):
    r = _plan(DKMS3_INSTALLED, spec=spec, kernel=kernel)
    assert r.returncode != 0 and r.stdout == "", (spec, kernel, r.stdout)
    assert r.stderr.strip()


def test_spec_check_ties_the_driver_fact_to_the_vendored_module():
    assert _bash('strih_nic_driver_spec_check "$SP" r8152', env={"SP": SPEC}).returncode == 0
    r = _bash('strih_nic_driver_spec_check "$SP" r8169', env={"SP": SPEC})
    assert r.returncode != 0 and "STRIH_NIC_DRIVER is 'r8169'" in r.stderr
    r = _bash("strih_nic_driver_spec_check realtek-r8152-2.21.3 r8152")
    assert r.returncode != 0 and "this tree vendors %s" % SPEC in r.stderr


# --- the apply step, with a fake dkms state machine ------------------------------------------------

FAKE_DKMS = r'''#!/bin/bash
# fake dkms: a state db of `pkg|ver|kernel|state` records; logs every call. Like real DKMS 3.x, a kernel
# has ONE active version (state `installed`): `install` makes the new version active and leaves the old
# one `built`, and `remove` deletes module files only on a kernel where the removed version is ACTIVE.
# `remove -k K` removes the version from that kernel only (the version goes with its last kernel);
# `remove --all` from every kernel. FAKE_DKMS_INSTALL_FAIL_AFTER_COPY=<ver> models DKMS's own depmod
# failing after the copy: the active module of that kernel is deleted and the install exits 6.
set -euo pipefail
echo "dkms $*" >> "$FAKE/calls.log"
db="$FAKE/dkms.db"; touch "$db"
cmd="$1"; shift
m=""; v=""; k=""; all=0
while [ "$#" -gt 0 ]; do
  case "$1" in -m) m="$2"; shift 2 ;; -v) v="$2"; shift 2 ;; -k) k="$2"; shift 2 ;; --all) all=1; shift ;; *) shift ;; esac
done
setrec() { grep -v "^$1|$2|$3|" "$db" > "$db.tmp" || true; mv "$db.tmp" "$db"; echo "$1|$2|$3|$4" >> "$db"; }
case "$cmd" in
  status)
    while IFS='|' read -r rm rv rk rs; do
      [ -z "$m" ] || [ "$rm" = "$m" ] || continue
      if [ -z "$rk" ]; then echo "$rm/$rv: $rs"; else echo "$rm/$rv, $rk, x86_64: $rs"; fi
    done < "$db"
    # a "added" record is shown only while no kernel record exists
    ;;
  add)
    [ -f "$STRIH_NIC_DRV_SRC_ROOT/$m-$v/dkms.conf" ] || { echo "no source" >&2; exit 2; }
    grep -q "^$m|$v|" "$db" && { echo "already added" >&2; exit 3; }
    echo "$m|$v||added" >> "$db" ;;
  build)
    [ "${FAKE_DKMS_BUILD_FAIL:-0}" = 0 ] || exit 10
    grep -v "^$m|$v||added$" "$db" > "$db.tmp" || true; mv "$db.tmp" "$db"
    setrec "$m" "$v" "$k" built ;;
  install)
    [ "${FAKE_DKMS_INSTALL_FAIL:-0}" = 0 ] || exit 6
    [ "${FAKE_DKMS_INSTALL_FAIL_KERNEL:-}" != "$k" ] || exit 6
    if [ "${FAKE_DKMS_INSTALL_FAIL_AFTER_COPY:-}" = "$v" ]; then
      rm -f "$STRIH_NIC_DRV_MODULES_ROOT/$k/updates/dkms/r8152.ko.zst"
      sed -i "s/^\($m|[^|]*|$k|\)installed$/\1built/" "$db"
      exit 6
    fi
    grep -q "^$m|$v|$k|installed$" "$db" && { echo "already installed" >&2; exit 5; }
    mkdir -p "$STRIH_NIC_DRV_MODULES_ROOT/$k/updates/dkms"
    printf 'v%s (2025/10/28)\n' "$v" > "$STRIH_NIC_DRV_MODULES_ROOT/$k/updates/dkms/r8152.ko.zst"
    grep -v "^$m|$v||added$" "$db" > "$db.tmp" || true; mv "$db.tmp" "$db"
    # the previously active version of this kernel stays built, no longer active
    sed -i "s/^\($m|[^|]*|$k|\)installed$/\1built/" "$db"
    setrec "$m" "$v" "$k" installed ;;
  remove)
    while IFS='|' read -r rm_ rv rk rs; do
      [ "$rm_" = "$m" ] && [ "$rv" = "$v" ] && [ "$rs" = installed ] || continue
      [ "$all" = 1 ] || [ "$rk" = "$k" ] || continue
      rm -f "$STRIH_NIC_DRV_MODULES_ROOT/$rk/updates/dkms/r8152.ko.zst"
    done < "$db"
    if [ "$all" = 1 ]; then
      grep -v "^$m|$v|" "$db" > "$db.tmp" || true; mv "$db.tmp" "$db"
    else
      [ -n "$k" ] || { echo "fake dkms: remove needs -k or --all" >&2; exit 64; }
      grep -v "^$m|$v|$k|" "$db" > "$db.tmp" || true; mv "$db.tmp" "$db"
      # the version goes with its last kernel
      grep -q "^$m|$v|[^|][^|]*|" "$db" || { grep -v "^$m|$v|" "$db" > "$db.tmp" || true; mv "$db.tmp" "$db"; }
    fi
    true ;;
  *) echo "fake dkms: unknown $cmd" >&2; exit 64 ;;
esac
'''

FAKE_MODINFO = r'''#!/bin/bash
# fake modinfo: a module file's content IS its version string.
echo "modinfo $*" >> "$FAKE/calls.log"
k=""; field=""; target=""
while [ "$#" -gt 0 ]; do
  case "$1" in -k) k="$2"; shift 2 ;; -F) field="$2"; shift 2 ;; *) target="$1"; shift ;; esac
done
[ "$field" = version ] || exit 1
if [ -f "$target" ]; then cat "$target"; exit 0; fi
r="$STRIH_NIC_DRV_MODULES_ROOT/$k"
for f in "$r/updates/dkms/$target.ko.zst" "$r/updates/$target.ko" "$r/updates/$target.ko.zst" \
         "$r/kernel/drivers/net/usb/$target.ko.zst"; do
  [ -f "$f" ] && { cat "$f"; exit 0; }
done
echo "modinfo: ERROR: Module $target not found." >&2; exit 1
'''

LOGGER = '#!/bin/bash\necho "%s $*" >> "$FAKE/calls.log"\nexit 0\n'


def _exe(path, body):
    path.write_text(body)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


class Box:
    """A fake strih box: a module tree, /usr/src, udev dir, backup dir and a PATH of fakes."""

    def __init__(self, tmp_path, plain=LOADED, headers=True, other_kernels=()):
        self.root = tmp_path
        self.fake = tmp_path / "fake"
        self.fake.mkdir()
        self.bin = tmp_path / "bin"
        self.bin.mkdir()
        self.mods = tmp_path / "lib-modules"
        self.src = tmp_path / "usr-src"
        self.udev = tmp_path / "udev"
        self.backup = tmp_path / "backup"
        for k in (KERNEL,) + tuple(other_kernels):
            kd = self.mods / k
            (kd / "kernel/drivers/net/usb").mkdir(parents=True)
            (kd / "kernel/drivers/net/usb/r8152.ko.zst").write_text("")  # in-tree: no version
            (kd / "updates").mkdir()
            if headers:
                (kd / "build").mkdir()
                (kd / "build/Makefile").write_text("# headers\n")
        if plain is not None:
            (self.mods / KERNEL / "updates/r8152.ko").write_text(plain + "\n")
        _exe(self.bin / "dkms", FAKE_DKMS)
        _exe(self.bin / "modinfo", FAKE_MODINFO)
        for name in ("depmod", "udevadm", "apt-get"):
            _exe(self.bin / name, LOGGER % name)
        for name in ("modprobe", "rmmod", "insmod"):
            _exe(self.bin / name, LOGGER % ("FORBIDDEN-" + name))

    def env(self, **extra):
        env = {"PATH": "%s:%s" % (self.bin, os.environ["PATH"]), "FAKE": str(self.fake),
               "STRIH_NIC_DRV_KERNEL": KERNEL, "STRIH_NIC_DRV_MODULES_ROOT": str(self.mods),
               "STRIH_NIC_DRV_SRC_ROOT": str(self.src), "STRIH_NIC_DRV_UDEV_DIR": str(self.udev),
               "STRIH_NIC_DRV_BACKUP_DIR": str(self.backup), "STRIH_NIC_DRV_SYSROOT": str(self.root / "sys")}
        env.update(extra)
        return env

    def apply(self, repo=REPO, spec=SPEC, nicdrv="r8152", **extra):
        return _bash('strih_nic_driver_apply "$R" "$SP" "$ND"',
                     env=self.env(R=str(repo), SP=spec, ND=nicdrv, **extra))

    def calls(self):
        p = self.fake / "calls.log"
        return p.read_text().splitlines() if p.exists() else []

    def db(self):
        p = self.fake / "dkms.db"
        return sorted(p.read_text().split()) if p.exists() else []


def _index(calls, prefix):
    return next(i for i, c in enumerate(calls) if c.startswith(prefix))


def test_apply_replaces_the_live_hand_copy_with_dkms_in_the_safe_order(tmp_path):
    box = Box(tmp_path)
    r = box.apply()
    assert r.returncode == 0, r.stdout + r.stderr
    calls = box.calls()
    assert "%s on %s: UPGRADE" % (SPEC, KERNEL) in r.stdout
    # the source reached /usr/src and verifies
    dst = box.src / SPEC
    assert {p.name for p in dst.iterdir()} == VENDORED_FILES | {"SHA256SUMS"}
    for name, digest in _sums().items():
        assert hashlib.sha256((dst / name).read_bytes()).hexdigest() == digest
    # order: add < build < (plain copy moved) < install < depmod
    add = _index(calls, "dkms add -m realtek-r8152 -v 2.21.4")
    build = _index(calls, "dkms build -m realtek-r8152 -v 2.21.4 -k %s" % KERNEL)
    install = _index(calls, "dkms install -m realtek-r8152 -v 2.21.4 -k %s" % KERNEL)
    assert add < build < install, calls
    assert any(c == "depmod -a %s" % KERNEL for c in calls[install:]), calls
    moved = r.stdout.index("moved the hand-copied")
    assert r.stdout.index("dkms build") < moved < r.stdout.index("dkms install"), r.stdout
    # the plain copy is kept aside, the DKMS copy is in place
    assert not (box.mods / KERNEL / "updates/r8152.ko").exists()
    assert (box.backup / KERNEL / "r8152.ko").read_text().strip() == LOADED
    assert (box.mods / KERNEL / "updates/dkms/r8152.ko.zst").exists()
    assert box.db() == ["realtek-r8152|2.21.4|%s|installed" % KERNEL]
    # the udev rule is installed, rules reloaded, never re-triggered
    assert (box.udev / "50-usb-realtek-net.rules").read_bytes() == (VENDOR / "50-usb-realtek-net.rules").read_bytes()
    assert "udevadm control --reload-rules" in calls
    assert not any(c.startswith("udevadm trigger") for c in calls)
    # never reloaded live
    assert not any(c.startswith("FORBIDDEN") for c in calls), calls
    assert "reload pending: next boot" in r.stdout


def test_a_second_run_is_a_noop(tmp_path):
    box = Box(tmp_path)
    assert box.apply().returncode == 0
    (box.fake / "calls.log").unlink()
    r = box.apply()
    assert r.returncode == 0, r.stdout + r.stderr
    assert "%s on %s: NOOP" % (SPEC, KERNEL) in r.stdout
    calls = box.calls()
    assert not [c for c in calls if re.match(r"dkms (add|build|install|remove)\b", c)], calls
    assert "udev rule %s current" % (box.udev / "50-usb-realtek-net.rules") in r.stdout
    assert not [c for c in calls if c.startswith("udevadm")], calls


def test_a_fresh_box_installs(tmp_path):
    box = Box(tmp_path, plain=None)
    r = box.apply()
    assert r.returncode == 0, r.stdout + r.stderr
    assert "%s on %s: INSTALL" % (SPEC, KERNEL) in r.stdout
    assert box.db() == ["realtek-r8152|2.21.4|%s|installed" % KERNEL]
    assert not box.backup.exists()


def test_a_failed_install_puts_the_hand_copy_back(tmp_path):
    box = Box(tmp_path)
    r = box.apply(FAKE_DKMS_INSTALL_FAIL="1")
    assert r.returncode != 0
    assert "dkms install for %s failed -- any hand-copied module is back in place" % KERNEL in r.stderr
    assert (box.mods / KERNEL / "updates/r8152.ko").read_text().strip() == LOADED
    calls = box.calls()
    install = _index(calls, "dkms install")
    assert "depmod -a %s" % KERNEL in calls[install:], calls
    assert not any(c.startswith("FORBIDDEN") for c in calls)


def test_a_failed_build_changes_no_file(tmp_path):
    box = Box(tmp_path)
    r = box.apply(FAKE_DKMS_BUILD_FAIL="1")
    assert r.returncode != 0
    assert "dkms build for %s failed" % KERNEL in r.stderr
    assert (box.mods / KERNEL / "updates/r8152.ko").exists()
    assert not box.backup.exists()
    assert not [c for c in box.calls() if c.startswith("dkms install")]


def test_a_failed_move_aside_puts_the_moved_copies_back(tmp_path):
    box = Box(tmp_path)
    (box.mods / KERNEL / "updates/r8152.ko.zst").write_text(LOADED + "\n")
    # the second copy cannot be moved: a directory already sits at its backup path
    blocker = box.backup / KERNEL / "r8152.ko.zst"
    blocker.mkdir(parents=True)
    (blocker / "keep").write_text("x")
    r = box.apply()
    assert r.returncode != 0
    assert "every moved copy is back in place" in r.stderr, r.stderr
    assert (box.mods / KERNEL / "updates/r8152.ko").read_text().strip() == LOADED
    assert (box.mods / KERNEL / "updates/r8152.ko.zst").exists()
    assert not [c for c in box.calls() if c.startswith("dkms install")], box.calls()
    assert "depmod -a %s" % KERNEL in box.calls()


def test_a_changed_udev_rule_is_never_installed_on_a_noop_run(tmp_path):
    box = Box(tmp_path)
    assert box.apply().returncode == 0
    (box.udev / "50-usb-realtek-net.rules").unlink()
    tree = tmp_path / "tree"
    vend = tree / "vendor" / "realtek-r8152"
    vend.mkdir(parents=True)
    for p in VENDOR.iterdir():
        (vend / p.name).write_bytes(p.read_bytes())
    (vend / "50-usb-realtek-net.rules").write_text("# tampered\n")
    r = box.apply(repo=tree)
    assert r.returncode != 0
    assert "does not match its SHA256SUMS" in r.stderr, r.stderr
    assert not (box.udev / "50-usb-realtek-net.rules").exists()


def test_an_unknown_hand_installed_module_refuses_before_anything_is_built(tmp_path):
    box = Box(tmp_path, plain="v2.19.2 (2024/01/01)")
    r = box.apply()
    assert r.returncode != 0
    assert "not the known 2.21.4 copy" in r.stderr, r.stderr
    assert not [c for c in box.calls() if c.startswith(("dkms build", "dkms install"))]
    assert (box.mods / KERNEL / "updates/r8152.ko").exists()


def test_every_other_installed_kernel_with_headers_gets_the_module(tmp_path):
    box = Box(tmp_path, other_kernels=("7.0.0-34-generic",))
    r = box.apply()
    assert r.returncode == 0, r.stdout + r.stderr
    assert box.db() == sorted(["realtek-r8152|2.21.4|%s|installed" % KERNEL,
                               "realtek-r8152|2.21.4|7.0.0-34-generic|installed"])


def test_a_kernel_without_headers_is_skipped_until_its_headers_arrive(tmp_path):
    # nothing can build for a kernel without headers; installing them runs the DKMS headers hook
    box = Box(tmp_path, other_kernels=("7.0.0-34-generic",))
    for f in (box.mods / "7.0.0-34-generic" / "build").iterdir():
        f.unlink()
    r = box.apply()
    assert r.returncode == 0, r.stdout + r.stderr
    assert box.db() == ["realtek-r8152|2.21.4|%s|installed" % KERNEL]


def _old_version(box, kernels):
    rows = []
    for k in kernels:
        rows.append("realtek-r8152|2.21.3|%s|installed" % k)
        (box.mods / k / "updates/dkms").mkdir(parents=True, exist_ok=True)
        (box.mods / k / "updates/dkms/r8152.ko.zst").write_text("v2.21.3 (2025/01/01)\n")
    (box.fake / "dkms.db").write_text("\n".join(rows) + "\n")


def _on_disk(box, k):
    f = box.mods / k / "updates/dkms/r8152.ko.zst"
    return f.read_text().split()[0] if f.exists() else None


def test_an_older_dkms_version_is_removed_only_after_every_install(tmp_path):
    other = "7.0.0-34-generic"
    box = Box(tmp_path, plain=None, other_kernels=(other,))
    _old_version(box, (KERNEL, other))
    r = box.apply()
    assert r.returncode == 0, r.stdout + r.stderr
    calls = box.calls()
    removes = [i for i, c in enumerate(calls) if c.startswith("dkms remove")]
    installs = [i for i, c in enumerate(calls) if c.startswith("dkms install")]
    assert len(installs) == 2 and removes and max(installs) < min(removes), calls
    assert sorted(calls[i] for i in removes) == sorted(
        "dkms remove -m realtek-r8152 -v 2.21.3 -k %s" % k for k in (KERNEL, other)), calls
    assert box.db() == sorted(["realtek-r8152|2.21.4|%s|installed" % KERNEL,
                               "realtek-r8152|2.21.4|%s|installed" % other])
    # the remove of the no-longer-active old version deleted no module file
    assert _on_disk(box, KERNEL) == "v2.21.4" and _on_disk(box, other) == "v2.21.4"


def test_a_failed_install_keeps_the_older_dkms_version_on_disk(tmp_path):
    box = Box(tmp_path, plain=None)
    _old_version(box, (KERNEL,))
    r = box.apply(FAKE_DKMS_INSTALL_FAIL="1")
    assert r.returncode != 0
    assert not [c for c in box.calls() if c.startswith("dkms remove")], box.calls()
    assert _on_disk(box, KERNEL) == "v2.21.3"
    assert "realtek-r8152|2.21.3|%s|installed" % KERNEL in box.db()


def test_a_failed_install_on_another_kernel_keeps_the_older_version_there(tmp_path):
    other = "7.0.0-34-generic"
    box = Box(tmp_path, plain=None, other_kernels=(other,))
    _old_version(box, (KERNEL, other))
    r = box.apply(FAKE_DKMS_INSTALL_FAIL_KERNEL=other)
    assert r.returncode != 0
    assert not [c for c in box.calls() if c.startswith("dkms remove")], box.calls()
    # the next boot into the other kernel still finds a driver that binds the RTL8157
    assert _on_disk(box, other) == "v2.21.3"
    assert _on_disk(box, KERNEL) == "v2.21.4"


def test_a_headerless_kernel_keeps_the_older_version_it_runs(tmp_path):
    # the old version stays on a kernel the vendored one could not be built for: a `--all` remove would
    # delete the only driver that kernel has, and booting it would bring up no rig NIC
    bare = "7.0.0-28-generic"
    box = Box(tmp_path, plain=None, other_kernels=(bare,))
    for f in (box.mods / bare / "build").iterdir():
        f.unlink()
    _old_version(box, (KERNEL, bare))
    r = box.apply()
    assert r.returncode == 0, r.stdout + r.stderr
    assert _on_disk(box, KERNEL) == "v2.21.4"
    assert _on_disk(box, bare) == "v2.21.3"
    assert "realtek-r8152|2.21.3|%s|installed" % bare in box.db()
    calls = box.calls()
    assert "dkms remove -m realtek-r8152 -v 2.21.3 -k %s" % KERNEL in calls, calls
    assert not [c for c in calls if c.startswith("dkms remove") and ("--all" in c or bare in c)], calls
    assert "WARNING: realtek-r8152/2.21.3 is kept for %s" % bare in r.stderr, r.stderr
    assert "install linux-headers-%s" % bare in r.stderr


def test_a_failed_install_after_dkms_copied_reinstalls_the_previous_version(tmp_path):
    box = Box(tmp_path, plain=None)
    _old_version(box, (KERNEL,))
    r = box.apply(FAKE_DKMS_INSTALL_FAIL_AFTER_COPY="2.21.4")
    assert r.returncode != 0
    calls = box.calls()
    failed = _index(calls, "dkms install -m realtek-r8152 -v 2.21.4 -k %s" % KERNEL)
    assert "dkms install -m realtek-r8152 -v 2.21.3 -k %s" % KERNEL in calls[failed + 1:], calls
    assert _on_disk(box, KERNEL) == "v2.21.3"
    assert "re-installed the previous realtek-r8152/2.21.3, %s now loads version '2.21.3'" % KERNEL in r.stderr
    assert not [c for c in calls if c.startswith("dkms remove")], calls


def test_missing_headers_are_installed_first_and_a_failed_install_stops_the_step(tmp_path):
    box = Box(tmp_path, headers=False)
    r = box.apply()
    # it asked apt for the headers; the fake apt-get installs nothing, so the step stops before DKMS
    assert "apt-get install -y linux-headers-%s" % KERNEL in box.calls(), box.calls()
    assert r.returncode != 0
    assert "headers of %s are still missing" % KERNEL in r.stderr, r.stderr
    assert not [c for c in box.calls() if c.startswith("dkms")], box.calls()
    assert (box.mods / KERNEL / "updates/r8152.ko").exists()


@pytest.mark.parametrize("spec,nicdrv,msg", [
    ("realtek-r8152-2.21.3", "r8152", "this tree vendors"),
    (SPEC, "r8169", "STRIH_NIC_DRIVER is 'r8169'"),
])
def test_apply_refuses_a_fact_it_cannot_serve(tmp_path, spec, nicdrv, msg):
    box = Box(tmp_path)
    r = box.apply(spec=spec, nicdrv=nicdrv)
    assert r.returncode != 0 and msg in r.stderr, r.stderr
    assert not [c for c in box.calls() if c.startswith("dkms")]


def test_an_install_without_the_staged_source_fails_loud(tmp_path):
    box = Box(tmp_path)
    tree = tmp_path / "tree"
    tree.mkdir()
    r = box.apply(repo=tree)
    assert r.returncode != 0
    assert "is not staged" in r.stderr, r.stderr
    assert (box.mods / KERNEL / "updates/r8152.ko").exists()


def test_a_changed_vendored_source_is_refused(tmp_path):
    tree = tmp_path / "tree"
    vend = tree / "vendor" / "realtek-r8152"
    vend.mkdir(parents=True)
    for p in VENDOR.iterdir():
        (vend / p.name).write_bytes(p.read_bytes())
    (vend / "r8152.c").write_text("/* tampered */\n")
    box = Box(tmp_path)
    r = box.apply(repo=tree)
    assert r.returncode != 0
    assert "does not match its SHA256SUMS" in r.stderr, r.stderr
    assert not [c for c in box.calls() if re.match(r"dkms (add|build|install|remove)\b", c)]


def test_the_lib_never_reloads_or_retriggers_on_a_code_line():
    for line in _code_lines(LIB):
        assert not re.search(r"\b(modprobe|rmmod|insmod)\b", line) or line.strip().startswith(("echo", "printf")), line
        assert "udevadm trigger" not in line, line


# --- verify-strih item 36, over a fake sysfs tree --------------------------------------------------

FAKE_NMCLI = r'''#!/bin/bash
# fake nmcli over a `uuid|name|ifname|addresses` table in $FAKE_NM
args="$*"
case "$args" in
  "-g UUID connection show") cut -d'|' -f1 "$FAKE_NM" ;;
  "-g connection.id connection show "*) awk -F'|' -v u="${args##* }" '$1 == u { print $2 }' "$FAKE_NM" ;;
  "-g connection.interface-name connection show "*) awk -F'|' -v u="${args##* }" '$1 == u { print $3 }' "$FAKE_NM" ;;
  "-g ipv4.addresses connection show "*) awk -F'|' -v u="${args##* }" '$1 == u { print $4 }' "$FAKE_NM" ;;
  *) echo "fake nmcli: $args" >&2; exit 2 ;;
esac
'''

GOOD_NM = "u-1|strih-lan|%s|10.77.9.202/23\n" % NIC


class Sysfs:
    """A fake /sys with the RTL8157 on usb2/2-3 (r8152), an onboard r8169 NIC and lo."""

    def __init__(self, tmp_path, usb="10000", link="5000", loaded=LOADED, nm=GOOD_NM, dkms_db=None):
        self.tmp = tmp_path
        s = tmp_path / "sys"
        self.sys = s
        usbdev = s / "devices/pci0000:00/0000:00:14.0/usb2/2-3"
        intf = usbdev / "2-3:1.0"
        net = intf / "net" / NIC
        net.mkdir(parents=True)
        if usb is not None:
            (usbdev / "speed").write_text(usb + "\n")
        (usbdev / "idVendor").write_text("0bda\n")
        if link is not None:
            (net / "speed").write_text(link + "\n")
        (s / "bus/usb/drivers/r8152").mkdir(parents=True)
        os.symlink(intf, net / "device")
        os.symlink(s / "bus/usb/drivers/r8152", intf / "driver")
        (s / "class/net").mkdir(parents=True)
        os.symlink(net, s / "class/net" / NIC)
        pci = s / "devices/pci0000:00/0000:03:00.0"
        net2 = pci / "net/enp3s0"
        net2.mkdir(parents=True)
        (net2 / "speed").write_text("1000\n")
        (s / "bus/pci/drivers/r8169").mkdir(parents=True)
        os.symlink(pci, net2 / "device")
        os.symlink(s / "bus/pci/drivers/r8169", pci / "driver")
        os.symlink(net2, s / "class/net/enp3s0")
        lo = s / "devices/virtual/net/lo"
        lo.mkdir(parents=True)
        os.symlink(lo, s / "class/net/lo")
        if loaded is not None:
            (s / "module/r8152").mkdir(parents=True)
            (s / "module/r8152/version").write_text(loaded + "\n")
        self.bin = tmp_path / "bin"
        self.bin.mkdir()
        self.fake = tmp_path / "fake"
        self.fake.mkdir()
        _exe(self.bin / "dkms", FAKE_DKMS)
        _exe(self.bin / "nmcli", FAKE_NMCLI)
        (self.fake / "dkms.db").write_text(dkms_db if dkms_db is not None else
                                            "realtek-r8152|2.21.4|%s|installed\n" % KERNEL)
        (tmp_path / "nm.tsv").write_text(nm)

    def grade(self, facts_env=None, drop=()):
        env = {"PATH": "%s:%s" % (self.bin, os.environ["PATH"]), "FAKE": str(self.fake),
               "FAKE_NM": str(self.tmp / "nm.tsv"), "STRIH_NIC_DRV_KERNEL": KERNEL,
               "STRIH_NIC_DRV_MODULES_ROOT": str(self.tmp / "no-modules")}
        for name in drop:
            (self.bin / name).unlink()
        if facts_env:
            env.update(facts_env)
        r = _bash('strih_nic_grade_rows "$SYSROOT" ""', env=dict(env, SYSROOT=str(self.sys)),
                  sources=(PROVISION, LIB))
        assert r.returncode == 0, r.stderr
        return [tuple(line.split("|", 1)) for line in r.stdout.splitlines()]


def _states(rows):
    return {d.split(")", 1)[0] + ")": s for s, d in rows}


def test_grade_passes_the_live_target_state(tmp_path):
    rows = Sysfs(tmp_path).grade()
    assert _states(rows) == {"(nic-driver)": "OK", "(nic-dkms)": "OK", "(nic-usb)": "OK", "(nic-link)": "OK",
                             "(nic-nm)": "OK"}, rows
    text = "\n".join(d for _, d in rows)
    assert "USB link 10000 Mb/s >= 10000" in text
    assert "Ethernet link 5000 Mb/s >= 5000" in text
    assert "strih-lan carrying 10.77.9.202 is pinned to %s" % NIC in text


def test_grade_fails_a_gen1_usb_link_and_names_the_connector_flip(tmp_path):
    rows = Sysfs(tmp_path, usb="5000").grade()
    usb = [d for s, d in rows if d.startswith("(nic-usb)")]
    assert _states(rows)["(nic-usb)"] == "FAIL", rows
    assert "USB link 5000 Mb/s < 10000" in usb[0]
    assert "flip it 180 degrees / use the USB-C port, USB-A is 5 Gb/s" in usb[0]


def test_grade_fails_a_2500_ethernet_link(tmp_path):
    rows = Sysfs(tmp_path, link="2500").grade()
    assert _states(rows)["(nic-link)"] == "FAIL", rows
    assert any("Ethernet link 2500 Mb/s < 5000" in d for _, d in rows)


@pytest.mark.parametrize("usb,link", [(None, "5000"), ("", "5000"), ("10000", None), ("10000", "-1"),
                                      ("10000", ""), ("fast", "5000")])
def test_an_unreadable_speed_is_never_a_pass(tmp_path, usb, link):
    rows = Sysfs(tmp_path, usb=usb, link=link).grade()
    st = _states(rows)
    kind = "(nic-usb)" if usb in (None, "", "fast") else "(nic-link)"
    assert st[kind] == "FAIL", rows


@pytest.mark.parametrize("loaded,why", [
    (None, "is not loaded or has no version"),
    ("", "is not loaded or has no version"),
    ("v2.19.2 (2024/01/01)", "loaded r8152 is 2.19.2, want 2.21.4"),
])
def test_grade_fails_a_driver_that_is_not_loaded_or_another_version(tmp_path, loaded, why):
    rows = Sysfs(tmp_path, loaded=loaded).grade()
    assert _states(rows)["(nic-driver)"] == "FAIL", rows
    assert any(why in d for _, d in rows), rows


@pytest.mark.parametrize("db", ["", "realtek-r8152|2.21.4|%s|built\n" % KERNEL,
                                "realtek-r8152|2.21.4|6.8.0-1-generic|installed\n"])
def test_grade_fails_when_dkms_does_not_have_it_for_the_running_kernel(tmp_path, db):
    rows = Sysfs(tmp_path, dkms_db=db).grade()
    assert _states(rows)["(nic-dkms)"] == "FAIL", rows


def test_grade_fails_when_dkms_is_absent(tmp_path):
    rows = Sysfs(tmp_path).grade(drop=("dkms",))
    assert _states(rows)["(nic-dkms)"] == "FAIL", rows


@pytest.mark.parametrize("nm,ok", [
    (GOOD_NM, True),
    # the replaced 2.5 GbE adapter's profile is dormant: its interface is gone
    ("u-0|strih-lan-old|enx6c1ff766154b|10.77.9.202/23\n" + GOOD_NM, True),
    # multiple addresses in the nmcli value
    ("u-1|strih-lan|%s|10.77.9.202/23, 192.168.5.2/24\n" % NIC, True),
    # unpinned: could come up on any interface
    ("u-1|strih-lan||10.77.9.202/23\n", False),
    ("u-9|stray||10.77.9.202/23\n" + GOOD_NM, False),
    # pinned to the onboard NIC, which is present
    ("u-1|strih-lan|enp3s0|10.77.9.202/23\n", False),
    # only a dormant profile carries the IP
    ("u-0|strih-lan-old|enx6c1ff766154b|10.77.9.202/23\n", False),
    # nothing carries the IP (a prefix match must not count)
    ("u-1|strih-lan|%s|10.77.9.2020/23\n" % NIC, False),
    ("", False),
])
def test_grade_nm_pinning(tmp_path, nm, ok):
    rows = Sysfs(tmp_path, nm=nm).grade()
    assert _states(rows)["(nic-nm)"] == ("OK" if ok else "FAIL"), rows


def test_grade_fails_when_nmcli_is_absent(tmp_path):
    rows = Sysfs(tmp_path).grade(drop=("nmcli",))
    assert _states(rows)["(nic-nm)"] == "FAIL", rows


def test_grade_notes_a_box_without_an_out_of_tree_driver_or_usb_nic(tmp_path):
    env = _fixture(tmp_path, [("STRIH_NIC_OOT_DRIVER=%s" % SPEC, "STRIH_NIC_OOT_DRIVER=none"),
                              ("STRIH_NIC_MIN_USB_MBPS=10000", "STRIH_NIC_MIN_USB_MBPS=none")])
    (tmp_path / "sysfs").mkdir()
    rows = Sysfs(tmp_path / "sysfs").grade(facts_env=env)
    st = _states(rows)
    assert st["(nic-driver)"] == "NOTE" and st["(nic-usb)"] == "NOTE", rows
    assert "(nic-dkms)" not in st
    assert st["(nic-link)"] == "OK" and st["(nic-nm)"] == "OK", rows


def test_grade_fails_when_the_rig_nic_cannot_be_resolved(tmp_path):
    s = Sysfs(tmp_path)
    (s.sys / "class/net" / NIC).unlink()
    rows = s.grade()
    assert ("FAIL", ) == tuple(st for st, d in rows if d.startswith("(nic) cannot resolve")), rows


def test_grade_report_dispatches_rows_and_a_silent_grader_fails():
    body = ('ok() { echo "OK:$1"; }; bad() { echo "BAD:$1"; }; note() { echo "NOTE:$1"; }\n'
            'strih_nic_grade_rows() { printf "OK|a\\nFAIL|b\\nNOTE|c\\n"; }\n'
            'strih_nic_grade_report /sys "" ; echo rc=$?\n'
            'strih_nic_grade_rows() { :; }\n'
            'strih_nic_grade_report /sys "" ; echo rc=$?\n')
    r = _bash(body)
    assert r.stdout.splitlines() == ["OK:a", "BAD:b", "NOTE:c", "rc=0", "rc=1"], r.stdout


@pytest.mark.parametrize("kind,val,mn,want", [
    ("usb", "10000", "10000", "OK"), ("usb", "20000", "10000", "OK"), ("usb", "5000", "10000", "FAIL"),
    ("usb", "480", "10000", "FAIL"), ("link", "5000", "5000", "OK"), ("link", "10000", "5000", "OK"),
    ("link", "2500", "5000", "FAIL"), ("link", "-1", "5000", "FAIL"), ("link", "", "5000", "FAIL"),
    ("link", "5000", "", "FAIL"), ("link", "5000", "none", "FAIL"), ("bogus", "5000", "5000", "FAIL"),
])
def test_speed_verdict(kind, val, mn, want):
    r = _bash('strih_nic_speed_verdict "$KD" "$V" "$M" enx0', env={"KD": kind, "V": val, "M": mn})
    assert r.stdout.split("|", 1)[0] == want, r.stdout
    assert (r.returncode == 0) == (want == "OK")


# --- the wiring ------------------------------------------------------------------------------------

def test_setup_strih_runs_step_1b_between_step_1_and_step_2():
    s = SETUP.read_text()
    assert '. "${HERE}/lib/strih-nic-driver.sh"' in s
    assert "TOTAL_STEPS=17" in s
    s1 = s.index('\nstep 1 "')
    s1b = s.index('\nstep "1b" "')
    s2 = s.index('\nstep 2 "')
    assert s1 < s1b < s2
    block = s[s1b:s2]
    assert 'strih_nic_driver_apply "${HERE}/.." "$NIC_OOT" "$(strih_lx_nic_driver)"' in block
    assert "NIC_OOT=\"$(strih_lx_nic_oot_driver)\"" in s[s1:s2]
    assert '[ "$NIC_OOT" = none ]' in block
    for line in block.splitlines():
        if not line.lstrip().startswith("#"):
            assert not re.search(r"\b(modprobe|rmmod)\b", line), line


def test_verify_strih_grades_item_36_before_item_35():
    v = VERIFY.read_text()
    assert '. "${HERE}/lib/strih-nic-driver.sh"' in v
    i36 = v.index("# 36) ")
    assert i36 < v.index("# 35) Downstream Keyer") < v.index("# 32) the shared OBS-box appliance baseline")
    block = v[i36:v.index("# 35) Downstream Keyer")]
    assert 'strih_nic_grade_report /sys "$(ip -o -4 addr show 2>/dev/null || true)"' in block
    assert "|| bad " in block


def test_the_deploy_archive_stages_the_vendored_driver():
    d = DEPLOY.read_text()
    assert "git -C \"$repo\" archive --format=tar HEAD scripts systemd intercom vendor/realtek-r8152 |" in d


def test_the_step_and_item_carry_no_box_identity_literal():
    # the strih-lx identity net (tests/strih_box_facts_1361.rs) scans these two for the fact values on a
    # code line; the module name must come from the facts / the lib, never a literal here.
    for path in (SETUP, VERIFY):
        for line in _code_lines(path):
            assert "r8152" not in line, (path.name, line)
