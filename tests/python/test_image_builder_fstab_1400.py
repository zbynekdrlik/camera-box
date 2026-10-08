#!/usr/bin/env python3
"""The image builders write their fstab through the read-only-root canon (issue 1400).

`scripts/lib/ro-root.sh` (issue 808) is the ONE read-only-root fstab canon: setup-device.sh STEP 18
and the handheld SBC write through it. The two image builders used to hand-type their own lines, and
build-image.sh's set had drifted (/var/log 64M, no size on /tmp and /var/tmp, no /var/spool). The
main design (comment 6052476615, Approach 1) moves both onto the canon and pins what each keeps on
purpose.

These tests pin:
  - build-image.sh: `build_image_fstab_text` (lifted from the script and run with the lib, the way
    test_grub_fast_boot_1394.py lifts install_bootloader -- the script is not sourceable, its
    top-level `trap cleanup EXIT` removes /tmp/camera-box-build) prints the golden: a header and
    the WHOLE canon tmpfs set. It carries NO root line, on purpose: the image's / is the overlayfs
    that configure_overlay's initramfs hook assembles, and a `UUID=... / ext4 ro` line would make
    systemd-remount-fs remount that overlay root read-only (nothing written through / would reach
    the overlay's upper layer).
  - create-usb-linux.sh: `create_usb_first_boot_fstab ROOT_UUID EFI_UUID` (called through the
    script's own CREATE_USB_SOURCE_ONLY=1 mode, fake UUIDs, no root) prints the golden. Its
    pre-setup differences are pinned: the root stays rw until STEP 18 rewrites the fstab, the EFI
    line and the issue-1309 journal-partition line stay, and /var/cache is the only tmpfs. The
    output is FIELD-identical to the pre-1400 fstab; only the /var/cache line's column padding
    changed.
  - no script under scripts/ other than scripts/lib/ro-root.sh hand-types an fstab-shaped tmpfs
    line for /tmp, /var/log, /var/tmp, /var/cache or /var/spool.
The STEP 18 golden (tests/fixtures/ro_root_fstab_808/) is pinned by test_ro_root_808.py, unchanged.

stdlib only (python-tests CI job, no toolchain). Runnable directly or under pytest.
"""
import os
import re
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
SCRIPTS = os.path.join(REPO, "scripts")
LIB = os.path.join(SCRIPTS, "lib", "ro-root.sh")
LOG_DIET = os.path.join(SCRIPTS, "lib", "log-diet.sh")
BUILD_IMAGE = os.path.join(SCRIPTS, "build-image.sh")
CREATE_USB = os.path.join(SCRIPTS, "create-usb-linux.sh")
GOLDEN = os.path.join(REPO, "tests", "fixtures", "image_builder_fstab_1400")

ROOT_UUID = "TESTROOTUUID"
EFI_UUID = "ABCD-1234"

# create-usb-linux.sh's first-boot fstab as origin/dev 1085770d6 wrote it (before issue 1400),
# rendered with the fake UUIDs above. The new output must keep every FIELD of it.
PRE_1400_CREATE_USB = (
    "UUID=TESTROOTUUID /         ext4  errors=remount-ro 0 1\n"
    "UUID=ABCD-1234  /boot/efi vfat  umask=0077        0 1\n"
    "tmpfs           /var/cache tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=512M 0 0\n"
    "LABEL=cambox-journal /var/log/journal ext4 rw,nofail,noatime,nosuid,nodev 0 2\n"
)

# An fstab-shaped tmpfs line for one of the canon's mount points: fs_spec `tmpfs` or `none` (tmpfs
# ignores it), the mount point (a trailing slash allowed), then fstype `tmpfs`. The fields may be
# separated by whitespace or a `\t` escape, and the line may start at a line start, whitespace, a
# quote or a `\n`/`\t` escape, so a heredoc line, a printf/echo string and a whole fstab in ONE
# format/string literal all match, while `mount -t tmpfs tmpfs /tmp` (no fstype after the path)
# does not.
_SEP = r"(?:\s|\\t)+"
HAND_TYPED_TMPFS = re.compile(
    r"""(?:^|[\s'"]|\\[nt])(?:tmpfs|none)""" + _SEP
    + r"(?:/tmp|/var/log|/var/tmp|/var/cache|/var/spool)/?" + _SEP
    + r"""tmpfs(?:\s|\\[nt]|['"]|$)"""
)


def _read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def _golden(name):
    return _read(os.path.join(GOLDEN, name))


def _bash(script, *args):
    """Run `script` with bash; every value goes in as a positional argument, never as script text."""
    return subprocess.run(["bash", "-c", script, "harness", *args], capture_output=True, text=True)


def _lib_out(snippet, *args):
    r = _bash('set -euo pipefail\n. "%s"\n. "%s"\n%s\n' % (LOG_DIET, LIB, snippet), *args)
    assert r.returncode == 0, (r.returncode, r.stderr)
    return r.stdout


def _function(text, name):
    """The text of a top-level shell function `name() {` through its closing `}` at column 0."""
    s = text.find("\n%s() {\n" % name)
    assert s >= 0, "%s() is not defined" % name
    e = text.find("\n}\n", s)
    assert e > s
    return text[s + 1 : e + 3]


def _code(text):
    return "\n".join(l for l in text.splitlines() if not l.lstrip().startswith("#"))


def _mount_lines(fstab):
    return [l for l in fstab.splitlines() if l.strip() and not l.lstrip().startswith("#")]


# =================================================================================================
# build-image.sh
# =================================================================================================
def _build_image_fstab():
    func = _function(_read(BUILD_IMAGE), "build_image_fstab_text")
    r = _bash('set -euo pipefail\n. "%s"\n%sbuild_image_fstab_text\n' % (LIB, func))
    assert r.returncode == 0 and not r.stderr, (r.returncode, r.stderr)
    return r.stdout


def test_build_image_sources_the_ro_root_lib():
    assert re.search(r'(?m)^\. "\$\{?SCRIPT_DIR\}?/lib/ro-root\.sh"', _read(BUILD_IMAGE))


def test_build_image_fstab_is_byte_identical_to_the_golden():
    assert _build_image_fstab() == _golden("build_image.fstab")


def test_build_image_fstab_carries_the_whole_canon_tmpfs_set():
    want = _lib_out("ro_root_tmpfs_lines").splitlines()
    assert _mount_lines(_build_image_fstab()) == want, "exactly the canon tmpfs set, in order"


def test_build_image_fstab_has_no_root_line_because_its_root_is_the_overlay():
    # The pinned decision (issue 1400): no `/` entry. It rests on the overlay root, so the overlay
    # must still be there; if configure_overlay ever goes, revisit the decision (a plain ext4 root
    # would then take the canon's ro_root_root_line).
    for line in _mount_lines(_build_image_fstab()):
        assert line.split()[1] != "/", "build-image.sh must not write a root line: " + line
    text = _read(BUILD_IMAGE)
    overlay = _function(text, "configure_overlay")
    assert "mount -t overlay overlay -o lowerdir=/mnt/root-ro" in overlay
    assert re.search(r"(?m)^\s+configure_overlay$", _code(_function(text, "main")))


def test_build_image_writes_the_fstab_through_the_function():
    boot = _code(_function(_read(BUILD_IMAGE), "bootstrap_rootfs"))
    assert 'build_image_fstab_text > "${WORK_DIR}/rootfs/etc/fstab"' in boot, boot
    assert 'cat > "${WORK_DIR}/rootfs/etc/fstab"' not in boot, "no hand-written fstab heredoc"


# =================================================================================================
# create-usb-linux.sh
# =================================================================================================
def _first_boot_fstab(root=ROOT_UUID, efi=EFI_UUID):
    """Source create-usb-linux.sh in its library-only mode (its own `set -euo pipefail` included)
    and call the first-boot fstab builder. The UUIDs are read into variables and the positional
    parameters cleared BEFORE the source: the script's argument parser reads (and shifts) them."""
    script = (
        'R="$1"; E="$2"; set --\n'
        "CREATE_USB_SOURCE_ONLY=1 source '%s'\n"
        'rc=0; create_usb_first_boot_fstab "$R" "$E" || rc=$?\n'
        'echo "rc=$rc"\n' % CREATE_USB
    )
    return _bash(script, root, efi)


def test_create_usb_sources_the_ro_root_lib():
    assert re.search(r'(?m)^\. "\$\{?SCRIPT_DIR\}?/lib/ro-root\.sh"', _read(CREATE_USB))


def _first_boot_fstab_text():
    r = _first_boot_fstab()
    assert r.returncode == 0 and not r.stderr, (r.returncode, r.stderr)
    assert r.stdout.endswith("rc=0\n"), r.stdout
    return r.stdout[: -len("rc=0\n")]


def test_create_usb_first_boot_fstab_is_byte_identical_to_the_golden():
    assert _first_boot_fstab_text() == _golden("create_usb_first_boot.fstab")


def test_create_usb_first_boot_fstab_is_field_identical_to_the_pre_1400_fstab():
    # Only the /var/cache line's column padding changed (the canon writes single spaces).
    new = _first_boot_fstab_text().splitlines()
    old = PRE_1400_CREATE_USB.splitlines()
    assert [l.split() for l in new] == [l.split() for l in old]


def test_create_usb_pins_its_pre_setup_differences_from_the_canon():
    mounts = _mount_lines(_first_boot_fstab_text())
    # 1. the root stays rw until setup-device.sh STEP 18 writes the read-only fstab
    root = [l for l in mounts if l.split()[1] == "/"]
    assert root == ["UUID=%s /         ext4  errors=remount-ro 0 1" % ROOT_UUID], root
    assert root[0].split()[3] == "errors=remount-ro", "the first-boot root is mounted rw"
    canon_root = _lib_out('ro_root_root_line "$1" ext4', ROOT_UUID).strip()
    assert root[0].split() != canon_root.split(), "the first-boot root must NOT be the ro canon"
    # 2. the EFI line and the issue-1309 journal-partition line stay
    assert "UUID=%s  /boot/efi vfat  umask=0077        0 1" % EFI_UUID in mounts
    assert _lib_out("log_diet_journal_fstab_line").strip() in mounts
    # 3. the only tmpfs is /var/cache, and it is the canon's line; STEP 18 adds the rest
    tmpfs = [l for l in mounts if l.split()[0] == "tmpfs"]
    assert tmpfs == [_lib_out("ro_root_tmpfs_line /var/cache").strip()], tmpfs


def test_create_usb_first_boot_fstab_refuses_an_empty_uuid():
    for root, efi in (("", EFI_UUID), (ROOT_UUID, ""), ("", "")):
        r = _first_boot_fstab(root, efi)
        assert r.stdout == "rc=1\n", (root, efi, r.stdout, r.stderr)


def test_create_usb_writes_the_fstab_through_the_function():
    conf = _code(_function(_read(CREATE_USB), "configure_system"))
    # The call and its named failure, as ONE statement: without the `|| error` continuation the
    # build would still stop (errexit), but with no message saying why.
    call = (
        'create_usb_first_boot_fstab "$ROOT_UUID" "$EFI_UUID" > "$MOUNT_ROOT/etc/fstab" \\\n'
        '        || error "issue 1400: no first-boot fstab written'
    )
    assert call in conf, conf
    assert 'cat > "$MOUNT_ROOT/etc/fstab"' not in conf, "no hand-written fstab heredoc"


# =================================================================================================
# the guard: nobody outside the canon hand-types a tmpfs line
# =================================================================================================
def test_guard_pattern_matches_the_hand_typed_shapes():
    for line in (
        "tmpfs           /var/log        tmpfs   defaults,noatime,nosuid,nodev,size=64M   0 0",
        "tmpfs /var/spool tmpfs defaults 0 0",
        "    echo 'tmpfs /tmp tmpfs defaults,size=100M 0 0' >> /etc/fstab",
        '    printf "%s\\n" "tmpfs /var/cache tmpfs defaults 0 0"',
        # a whole fstab in ONE format/string literal: the line starts after a `\n` escape
        "    printf 'UUID=%s / ext4 ro 0 1\\ntmpfs /tmp tmpfs defaults,size=100M 0 0\\n' \"$u\"",
        'FSTAB = "UUID=x / ext4 ro 0 1\\ntmpfs /var/log tmpfs defaults 0 0\\n"',
        # `\t` escapes or real tabs between the fields
        "printf 'tmpfs\\t/var/tmp\\ttmpfs\\tdefaults 0 0\\n'",
        "tmpfs\t/var/spool\ttmpfs\tdefaults\t0 0",
        # any fs_spec (tmpfs ignores it; `none` is common), a trailing slash on the mount point
        "none /var/log tmpfs defaults,size=50M 0 0",
        "tmpfs /var/log/ tmpfs defaults 0 0",
    ):
        assert HAND_TYPED_TMPFS.search(line), line
    for line in (
        "mount -t tmpfs tmpfs /tmp",
        "mount -t tmpfs none /tmp",
        "tmpfs /var/lib tmpfs defaults 0 0",
        "none /var/lib tmpfs defaults 0 0",
        "tmpfs /tmpx tmpfs defaults 0 0",
        "tmpfs /tmp/x tmpfs defaults 0 0",
        "proc /proc proc defaults 0 0",
        "$(ro_root_tmpfs_line /var/cache)",
    ):
        assert not HAND_TYPED_TMPFS.search(line), line


def test_no_script_outside_the_canon_hand_types_a_tmpfs_mount_line():
    offenders = []
    for root, _dirs, files in os.walk(SCRIPTS):
        for name in files:
            path = os.path.join(root, name)
            if os.path.samefile(path, LIB):
                continue
            try:
                text = _read(path)
            except UnicodeDecodeError:
                continue  # a binary fixture is no script
            for n, line in enumerate(text.splitlines(), 1):
                if line.lstrip().startswith("#"):
                    continue  # a comment is no mount line
                if HAND_TYPED_TMPFS.search(line):
                    offenders.append("%s:%d: %s" % (os.path.relpath(path, REPO), n, line.strip()))
    assert not offenders, (
        "write the tmpfs mount lines through scripts/lib/ro-root.sh (ro_root_tmpfs_line / "
        "ro_root_tmpfs_lines), never by hand (issue 1400):\n" + "\n".join(offenders)
    )


if __name__ == "__main__":
    import sys

    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    failed = 0
    for fn in fns:
        try:
            fn()
            print("ok   %s" % fn.__name__)
        except Exception as e:  # noqa: BLE001 - runner surfaces the failure, never swallows it
            failed += 1
            print("FAIL %s: %s" % (fn.__name__, e))
    print("\n%d/%d passed" % (len(fns) - failed, len(fns)))
    sys.exit(1 if failed else 0)
