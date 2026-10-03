#!/usr/bin/env python3
"""The ONE read-only-root canon shared by the cambox and the handheld SBC (issue 808, slice B).

Owner ruling 5948648089 (2.10.2026): the handheld SBC's root goes read-only once development ends,
"the same as the camboxes". The main design (comment 5971558113, Approach 1) puts the cambox canon
-- `setup-device.sh` STEP 18's read-only root line + its five tmpfs mounts -- into ONE pure lib,
`scripts/lib/ro-root.sh`, that STEP 18 and `bkshading-provision-sbc.sh --install` both use.

These tests pin:
  - the lib's pure functions (the tmpfs set + order, each line byte-for-byte, the root line, the
    first-token root-mode reading, the SBC whole-fstab builder that keeps a board's own mounts);
  - PARITY of `ro_root_mount_mode` with `setup-device.sh`'s own `root_mount_is_readonly`;
  - the GOLDEN: STEP 18's heredoc, lifted verbatim and run with the lib, writes byte-for-byte the
    fstab it wrote before the lib existed (`tests/fixtures/ro_root_fstab_808/`, captured from
    `origin/dev` 2c5dfd0b6), with and without the EFI and journal-partition lines;
  - STEP 18 really sources + calls the lib (no literal tmpfs line left in it).

stdlib only (python-tests CI job, no toolchain). Runnable directly or under pytest.
"""
import os
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "ro-root.sh")
LOG_DIET = os.path.join(REPO, "scripts", "lib", "log-diet.sh")
SETUP = os.path.join(REPO, "scripts", "setup-device.sh")
VERIFY = os.path.join(REPO, "scripts", "verify-device.sh")
GOLDEN = os.path.join(REPO, "tests", "fixtures", "ro_root_fstab_808")

TMPFS = [
    ("/tmp", "tmpfs /tmp tmpfs defaults,noatime,nosuid,nodev,mode=1777,size=100M 0 0"),
    ("/var/log", "tmpfs /var/log tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=50M 0 0"),
    ("/var/tmp", "tmpfs /var/tmp tmpfs defaults,noatime,nosuid,nodev,mode=1777,size=50M 0 0"),
    ("/var/cache", "tmpfs /var/cache tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=512M 0 0"),
    ("/var/spool", "tmpfs /var/spool tmpfs defaults,noatime,nosuid,nodev,mode=0755,size=10M 0 0"),
]
EFI_LINE = "UUID=ABCD-1234 /boot/efi vfat umask=0077 0 1"


def _src(snippet, **env):
    """Source the lib under set -euo pipefail, run `snippet`; returns CompletedProcess."""
    e = dict(os.environ)
    e.update(env)
    src = 'set -euo pipefail\n. "%s"\n%s\n' % (LIB, snippet)
    return subprocess.run(["bash", "-c", src], capture_output=True, text=True, env=e)


def _out(snippet, **env):
    r = _src(snippet, **env)
    assert r.returncode == 0, (r.returncode, r.stderr)
    return r.stdout


# ---------------------------------------------------------------------------------------------
# the lib itself
# ---------------------------------------------------------------------------------------------
def test_lib_parses_and_is_source_only():
    assert os.path.isfile(LIB), "missing %s" % LIB
    r = subprocess.run(["bash", "-n", LIB], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    body = open(LIB, encoding="utf-8").read()
    code = "\n".join(l for l in body.splitlines() if not l.lstrip().startswith("#"))
    assert "set -e" not in code, "a source-only lib must not set -e (it leaks into the caller)"
    # STEP 18's issue-1311 heredoc test runs it with `grep` stubbed out: the lib must not use it.
    for tool in ("grep", "awk", "sed"):
        assert tool not in code.split(), "ro-root.sh must not depend on %s" % tool


def test_tmpfs_paths_are_the_cambox_set_in_order():
    assert _out("ro_root_tmpfs_paths").split() == [p for p, _ in TMPFS]


def test_each_tmpfs_line_is_the_cambox_line_byte_for_byte():
    for path, line in TMPFS:
        assert _out('ro_root_tmpfs_line "$P"', P=path) == line + "\n", path
    assert _out("ro_root_tmpfs_lines") == "".join(l + "\n" for _, l in TMPFS)


def test_unknown_tmpfs_path_prints_nothing_and_fails():
    r = _src('ro_root_tmpfs_line /var/lib || echo "rc=$?"')
    assert r.stdout == "rc=1\n", r.stdout


def test_root_line_is_read_only_and_refuses_an_empty_field():
    assert _out("ro_root_root_line TESTUUID ext4") == "UUID=TESTUUID / ext4 ro 0 1\n"
    for args in ('"" ext4', "TESTUUID ''", ""):
        r = _src("ro_root_root_line %s || echo rc=$?" % args)
        assert r.stdout == "rc=1\n", (args, r.stdout)


MODES = [
    ("ro", "ro"),
    ("ro,relatime", "ro"),
    ("ro,noatime,commit=120,errors=remount-ro", "ro"),
    ("rw", "rw"),
    ("rw,relatime", "rw"),
    ("rw,noatime,commit=120,errors=remount-ro", "rw"),
    ("", "unknown"),
    ("rox,relatime", "unknown"),
    ("relatime,ro", "unknown"),
    ("ssh: connect to host x port 22: Connection refused", "unknown"),
]


def test_mount_mode_reads_the_first_token_only():
    for opts, want in MODES:
        assert _out('ro_root_mount_mode "$O"', O=opts) == want + "\n", opts


def test_mount_mode_agrees_with_both_cambox_root_mount_is_readonly():
    # setup-device.sh's root_mount_is_readonly delegates to the lib; verify-device.sh keeps its own
    # copy of the same contract. Both scripts' BASH_SOURCE guards make sourcing safe. For each,
    # ro_root_mount_mode says `ro` exactly when root_mount_is_readonly returns 0.
    for script in (SETUP, VERIFY):
        for opts, _want in MODES:
            src = (
                'set -uo pipefail\n. "%s"\n. "%s"\n'
                'if root_mount_is_readonly "$O"; then a=ro; else a=not; fi\n'
                'b="$(ro_root_mount_mode "$O")"; [ "$b" = ro ] || b=not\n'
                'echo "$a $b"\n' % (script, LIB)
            )
            r = subprocess.run(["bash", "-c", src], capture_output=True, text=True,
                               env=dict(os.environ, O=opts))
            a, b = r.stdout.split()
            assert a == b, "parity broken for %r: %s=%s lib=%s" % (opts, script, a, b)


def test_setup_device_reads_the_root_through_the_lib():
    body = open(SETUP, encoding="utf-8").read()
    start = body.index("root_mount_is_readonly() {")
    fn = body[start:body.index("\n}\n", start)]
    assert "ro_root_mount_mode" in fn, "setup-device.sh must not keep its own copy of the reading"


# ---------------------------------------------------------------------------------------------
# the SBC whole-fstab builder
# ---------------------------------------------------------------------------------------------
ARMBIAN_FSTAB = (
    "UUID=7c1e2c5a-0000-4b6c-9a1d-123456789abc / ext4 defaults,noatime,commit=120,errors=remount-ro 0 1\n"
    "tmpfs /tmp tmpfs defaults,nosuid 0 0\n"
)
PIOS_FSTAB = (
    "proc            /proc           proc    defaults          0       0\n"
    "PARTUUID=1234abcd-01  /boot/firmware  vfat    defaults          0       2\n"
    "PARTUUID=1234abcd-02  /               ext4    defaults,noatime  0       1\n"
    "# a swapfile is not a swap partition, use  dphys-swapfile swap[on|off]  for that\n"
)


def _fstab_text(uuid, fstype, original):
    r = _src('ro_root_fstab_text "$U" "$F" "$ORIG"', U=uuid, F=fstype, ORIG=original)
    return r


def test_sbc_fstab_is_ro_root_plus_the_cambox_tmpfs_set():
    r = _fstab_text("7c1e2c5a-0000-4b6c-9a1d-123456789abc", "ext4", ARMBIAN_FSTAB)
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    mounts = [l for l in lines if l and not l.startswith("#")]
    assert mounts[0] == "UUID=7c1e2c5a-0000-4b6c-9a1d-123456789abc / ext4 ro 0 1", mounts
    assert mounts[1:] == [l for _, l in TMPFS], "exactly the cambox tmpfs set, in order:\n" + r.stdout
    # the original rw root line and Armbian's own /tmp line are replaced, never kept
    assert "defaults,noatime,commit=120" not in r.stdout
    assert "tmpfs /tmp tmpfs defaults,nosuid 0 0" not in r.stdout


def test_sbc_fstab_keeps_a_boards_own_mounts_verbatim():
    r = _fstab_text("ROOTUUID", "ext4", PIOS_FSTAB)
    assert r.returncode == 0, r.stderr
    assert "PARTUUID=1234abcd-01  /boot/firmware  vfat    defaults          0       2\n" in r.stdout
    assert "proc            /proc           proc    defaults          0       0\n" in r.stdout
    assert "PARTUUID=1234abcd-02" not in r.stdout, "the original root entry is replaced"
    assert "dphys-swapfile" not in r.stdout, "original comments are not carried over"


def test_sbc_fstab_is_idempotent():
    first = _fstab_text("ROOTUUID", "ext4", PIOS_FSTAB).stdout
    second = _fstab_text("ROOTUUID", "ext4", first).stdout
    assert second == first


def test_sbc_fstab_refuses_an_empty_uuid():
    r = _src('ro_root_fstab_text "" ext4 "$ORIG" || echo rc=$?', ORIG=ARMBIAN_FSTAB)
    assert r.stdout == "rc=1\n", r.stdout


# ---------------------------------------------------------------------------------------------
# setup-device.sh STEP 18 uses the lib, and writes byte-for-byte what it wrote before
# ---------------------------------------------------------------------------------------------
def _step18_block():
    body = open(SETUP, encoding="utf-8").read().splitlines()
    start = next(i for i, l in enumerate(body) if l.startswith("cat > /etc/fstab << FSTABEOF"))
    end = next(j for j in range(start + 1, len(body)) if body[j] == "FSTABEOF")
    return "\n".join(body[start : end + 1]).replace("cat > /etc/fstab", "cat", 1)


def _run_step18(efi, journal):
    src = "set -euo pipefail\nROOT_UUID=TESTUUID\n"
    src += '. "%s"\n. "%s"\n' % (LOG_DIET, LIB)
    src += "blkid() { return 0; }\n" if journal else "blkid() { return 1; }\n"
    src += ("grep() { printf '%%s\\n' '%s'; }\n" % EFI_LINE) if efi else "grep() { return 1; }\n"
    src += _step18_block() + "\n"
    return subprocess.run(["bash", "-c", src], capture_output=True, text=True)


def test_step18_fstab_is_byte_identical_to_the_golden():
    for name, efi, journal in (
        ("cambox_efi_journal.fstab", True, True),
        ("cambox_bare.fstab", False, False),
    ):
        r = _run_step18(efi, journal)
        assert r.returncode == 0 and not r.stderr, (name, r.returncode, r.stderr)
        want = open(os.path.join(GOLDEN, name), encoding="utf-8").read()
        assert r.stdout == want, "STEP 18 output drifted from %s:\n%s" % (name, r.stdout)


def test_step18_refuses_an_empty_root_uuid():
    body = open(SETUP, encoding="utf-8").read().splitlines()
    start = next(i for i, l in enumerate(body) if l.startswith("cat > /etc/fstab << FSTABEOF"))
    guard = body[start - 1]
    assert guard.startswith('[ -n "$ROOT_UUID" ] || fail '), guard
    src = (
        "set -euo pipefail\nROOT_UUID=\n"
        'fail() { echo "FAIL: $*" >&2; exit 1; }\n'
        '. "%s"\n. "%s"\nblkid() { return 1; }\ngrep() { return 1; }\n%s\n%s\n'
        % (LOG_DIET, LIB, guard, _step18_block())
    )
    r = subprocess.run(["bash", "-c", src], capture_output=True, text=True)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert r.stdout == "", "no fstab may be written without a root UUID:\n" + r.stdout
    assert "root filesystem UUID" in r.stderr, r.stderr


def test_step18_sources_the_lib_and_calls_it_for_every_line():
    body = open(SETUP, encoding="utf-8").read()
    assert '. "$HERE/lib/ro-root.sh"' in body, "setup-device.sh must source scripts/lib/ro-root.sh"
    block = _step18_block()
    assert '$(ro_root_root_line "$ROOT_UUID" ext4)' in block, block
    for path, line in TMPFS:
        assert "$(ro_root_tmpfs_line %s)" % path in block, "STEP 18 must call the lib for " + path
        assert line not in block, "STEP 18 must not keep a literal copy of the %s line" % path


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
