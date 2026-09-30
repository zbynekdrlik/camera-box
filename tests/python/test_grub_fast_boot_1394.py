#!/usr/bin/env python3
"""#1394 -- a cambox boots straight to Linux: no GRUB menu and no countdown in ANY cambox image.

Owner ROZHODNUTÉ issuecomment-5913502328 (30.9.2026): cam6 sat in the GRUB menu with a 30 s
recordfail countdown during a reflash. The provisioned M.2 was already fast (setup-device.sh STEP 10
set timeout 0 / style hidden / recordfail timeout 0), but the create-usb-linux.sh base image wrote
GRUB_TIMEOUT=3 with no style and no recordfail timeout, and masks grub-common, so the recordfail
flag GRUB sets on every boot is never cleared. Ubuntu's 00_header then renders
`set timeout=${GRUB_RECORDFAIL_TIMEOUT:-30}` with the menu shown after any cut boot.

The fix (design issuecomment-5913515814, Approach 1): ONE declaration, scripts/lib/grub-fast-boot.sh,
used by create-usb-linux.sh (in the chroot), build-image.sh, setup-device.sh STEP 10, and graded on
the box by verify-device.sh check (aq) over the GENERATED /boot/grub/grub.cfg.

Fixtures: tests/fixtures/grub_fast_boot_1394/cam1-grub.cfg is cam1's live /boot/grub/grub.cfg (read
read-only 30.9.2026, filesystem UUID replaced by a placeholder). The two slow fixtures swap in the
timeout block Ubuntu 24.04's own 00_header make_timeout (grub-common 2.12-1ubuntu7.3) renders for
the base image (GRUB_TIMEOUT=3, no style, no recordfail timeout) and for a hidden 0 s menu whose
recordfail timeout is left at the 30 s default.

Tier-0: stdlib + bash only, no rig, no network.
"""
import os
import re
import stat
import subprocess
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "grub-fast-boot.sh")
CREATE_USB = os.path.join(REPO, "scripts", "create-usb-linux.sh")
BUILD_IMAGE = os.path.join(REPO, "scripts", "build-image.sh")
SETUP_DEVICE = os.path.join(REPO, "scripts", "setup-device.sh")
VERIFY_DEVICE = os.path.join(REPO, "scripts", "verify-device.sh")
FIXTURES = os.path.join(REPO, "tests", "fixtures", "grub_fast_boot_1394")

WANT_LINES = "GRUB_TIMEOUT=0\nGRUB_TIMEOUT_STYLE=hidden\nGRUB_RECORDFAIL_TIMEOUT=0\n"

# The recordfail_broken block Ubuntu's 00_header appends when GRUB cannot write grubenv.
RECORDFAIL_BROKEN_BLOCK = (
    "if [ $grub_platform = efi ]; then\n"
    "  set timeout=30\n"
    "  if [ x$feature_timeout_style = xy ] ; then\n"
    "    set timeout_style=menu\n"
    "  fi\n"
    "fi\n"
)


def _read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def _fixture(name):
    return _read(os.path.join(FIXTURES, name))


def _code(text):
    """The non-comment lines of a shell text."""
    return "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))


def _bash(script, env=None):
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True,
                          env={**os.environ, **(env or {})}, timeout=60)


def _lib(snippet, env=None):
    """Source the lib (no strict mode of our own, so a leak would show) and run <snippet>."""
    return _bash(f'. "{LIB}"\n{snippet}', env=env)


def _verdict(cfg_text):
    r = _lib('grub_fast_boot_cfg_verdict "$CFG"', env={"CFG": cfg_text})
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def _apply(path):
    return _lib(f'grub_fast_boot_apply "{path}"')


# =================================================================================================
# The lib: the ONE declaration
# =================================================================================================

def test_lib_prints_the_three_fast_boot_lines():
    r = _lib("grub_fast_boot_default_lines")
    assert r.returncode == 0, r.stderr
    assert r.stdout == WANT_LINES


def test_lib_constants_are_the_one_declaration():
    r = _lib('printf "%s|%s|%s\\n" "$GRUB_FAST_BOOT_TIMEOUT" "$GRUB_FAST_BOOT_STYLE" '
             '"$GRUB_FAST_BOOT_RECORDFAIL_TIMEOUT"')
    assert r.stdout.strip() == "0|hidden|0"


def test_lib_is_source_only_and_leaks_no_strict_mode():
    text = _read(LIB)
    assert ("# airuleset:script-ok source-only lib -- set -euo pipefail would leak into the "
            "sourcing shell") in text.splitlines()
    assert not re.search(r"(?m)^\s*set\s+-[a-z]*e", text), "a source-only lib never sets -e"
    r = _lib('printf "%s\\n" "$-"')
    assert "e" not in r.stdout.strip() and "u" not in r.stdout.strip()


def test_apply_appends_every_missing_key_and_keeps_every_other_line(tmp_path):
    stock = ('GRUB_DEFAULT=saved\nGRUB_DISTRIBUTOR="Ubuntu"\nGRUB_CMDLINE_LINUX_DEFAULT=""\n'
             'GRUB_CMDLINE_LINUX="console=tty0"\nGRUB_TERMINAL="console"\n'
             '#GRUB_TIMEOUT_STYLE=menu\nGRUB_DISABLE_OS_PROBER=true\n')
    f = tmp_path / "grub"
    f.write_text(stock)
    r = _apply(f)
    assert r.returncode == 0, r.stderr
    assert f.read_text() == stock + WANT_LINES


def test_apply_rewrites_wrong_values_in_place(tmp_path):
    wrong = ('GRUB_DEFAULT=saved\n'
             'GRUB_TIMEOUT=3\n'
             'GRUB_TIMEOUT_STYLE="menu"\n'
             '# GRUB_TIMEOUT=10 stays a comment\n'
             'GRUB_CMDLINE_LINUX="console=tty0 isolcpus=3"\n'
             '  GRUB_RECORDFAIL_TIMEOUT=30\n'
             'export GRUB_TIMEOUT=5\n'
             'GRUB_DISABLE_OS_PROBER=true\n')
    f = tmp_path / "grub"
    f.write_text(wrong)
    r = _apply(f)
    assert r.returncode == 0, r.stderr
    assert f.read_text() == ('GRUB_DEFAULT=saved\n'
                             'GRUB_TIMEOUT=0\n'
                             'GRUB_TIMEOUT_STYLE=hidden\n'
                             '# GRUB_TIMEOUT=10 stays a comment\n'
                             'GRUB_CMDLINE_LINUX="console=tty0 isolcpus=3"\n'
                             'GRUB_RECORDFAIL_TIMEOUT=0\n'
                             'GRUB_TIMEOUT=0\n'
                             'GRUB_DISABLE_OS_PROBER=true\n')


def test_apply_never_touches_a_same_prefix_decoy(tmp_path):
    decoys = 'GRUB_TIMEOUT_BUTTON=4\nGRUB_TIMEOUT_STYLE_BUTTON=menu\nMY_GRUB_TIMEOUT=9\n'
    f = tmp_path / "grub"
    f.write_text(decoys)
    assert _apply(f).returncode == 0
    assert f.read_text() == decoys + WANT_LINES


def test_apply_leaves_a_correct_file_byte_identical_and_unwritten(tmp_path):
    # No trailing newline on purpose: a rewrite would add one.
    correct = ('GRUB_DEFAULT=saved\nGRUB_TIMEOUT=0\nGRUB_TIMEOUT_STYLE=hidden\n'
               'GRUB_RECORDFAIL_TIMEOUT=0\nGRUB_CMDLINE_LINUX="console=tty0"')
    f = tmp_path / "grub"
    f.write_bytes(correct.encode())
    past = time.time() - 86400
    os.utime(f, (past, past))
    before = os.stat(f)
    r = _apply(f)
    assert r.returncode == 0, r.stderr
    after = os.stat(f)
    assert f.read_bytes() == correct.encode()
    assert after.st_mtime == before.st_mtime, "an already-correct file is not rewritten"
    assert after.st_ino == before.st_ino


def test_apply_is_idempotent_and_keeps_inode_and_mode(tmp_path):
    f = tmp_path / "grub"
    f.write_text("GRUB_DEFAULT=saved\nGRUB_TIMEOUT=3\n")
    os.chmod(f, 0o644)
    ino = os.stat(f).st_ino
    assert _apply(f).returncode == 0
    once = f.read_bytes()
    assert os.stat(f).st_ino == ino
    assert stat.S_IMODE(os.stat(f).st_mode) == 0o644
    past = time.time() - 3600
    os.utime(f, (past, past))
    assert _apply(f).returncode == 0
    assert f.read_bytes() == once
    assert os.stat(f).st_mtime == past


def test_apply_fails_loud_on_a_missing_file(tmp_path):
    missing = tmp_path / "nope" / "grub"
    r = _apply(missing)
    assert r.returncode != 0
    assert str(missing) in r.stderr
    assert not missing.exists()


def test_is_applied_truth_table(tmp_path):
    cases = [
        (WANT_LINES, 0),
        ("GRUB_DEFAULT=saved\n" + WANT_LINES, 0),
        ("GRUB_TIMEOUT=0\nGRUB_TIMEOUT_STYLE=hidden\n", 1),              # recordfail key missing
        (WANT_LINES + "GRUB_RECORDFAIL_TIMEOUT=30\n", 1),                  # a later line overrides
        (WANT_LINES.replace("hidden", '"hidden"'), 1),                    # not the literal line
    ]
    for i, (text, want) in enumerate(cases):
        f = tmp_path / f"g{i}"
        f.write_text(text)
        assert _lib(f'grub_fast_boot_is_applied "{f}"').returncode == want, text
    assert _lib(f'grub_fast_boot_is_applied "{tmp_path / "absent"}"').returncode == 1


# =================================================================================================
# The verdict over a GENERATED grub.cfg (verify-device check (aq), the image builders)
# =================================================================================================

def test_verdict_passes_the_live_fast_cam1_cfg():
    assert _verdict(_fixture("cam1-grub.cfg")) == "ok"


def test_verdict_fails_the_base_image_cfg_with_the_30s_recordfail_countdown():
    v = _verdict(_fixture("base-image-grub.cfg"))
    assert v.startswith("FAIL: ")
    assert "recordfail timeout is 30 s" in v
    assert "menu timeout 3" in v
    assert "timeout_style menu" in v


def test_verdict_fails_a_hidden_menu_whose_recordfail_timeout_is_30s():
    v = _verdict(_fixture("hidden-recordfail30-grub.cfg"))
    assert v.startswith("FAIL: ")
    assert "recordfail timeout is 30 s" in v
    assert "menu timeout" not in v and "timeout_style" not in v


def test_verdict_fails_an_empty_cfg():
    for empty in ("", "\n  \n"):
        v = _verdict(empty)
        assert v.startswith("FAIL: ") and "empty or unreadable" in v


def test_verdict_fails_a_cfg_without_the_recordfail_branch():
    fast = _fixture("cam1-grub.cfg")
    cut = fast.replace('if [ "${recordfail}" = 1 ] ; then\n  set timeout=0\nelse\n', "if true ; then\n")
    assert cut != fast
    v = _verdict(cut)
    assert v.startswith("FAIL: ") and "no recordfail branch" in v


def test_verdict_fails_the_recordfail_broken_efi_block():
    fast = _fixture("cam1-grub.cfg")
    marker = "### END /etc/grub.d/00_header ###"
    v = _verdict(fast.replace(marker, RECORDFAIL_BROKEN_BLOCK + marker))
    assert v.startswith("FAIL: ")
    assert "menu timeout 30" in v and "timeout_style menu" in v


def test_verdict_fails_a_countdown_style():
    fast = _fixture("cam1-grub.cfg")
    v = _verdict(fast.replace("set timeout_style=hidden", "set timeout_style=countdown"))
    assert v.startswith("FAIL: ") and "timeout_style countdown" in v


def test_verdict_ignores_commented_timeouts():
    fast = _fixture("cam1-grub.cfg")
    assert _verdict(fast + "# set timeout=30\n#set timeout_style=menu\n") == "ok"


# Review round 1 (#1394): the shapes the first verdict mis-graded or left untested.

def test_verdict_fails_an_empty_timeout_the_menu_that_waits_forever():
    # What Ubuntu's make_timeout renders for GRUB_TIMEOUT unset + style hidden: `set timeout=`.
    # GRUB reads an empty timeout as "no timeout", drops the hidden style and waits forever.
    fast = _fixture("cam1-grub.cfg")
    empty = fast.replace("    set timeout_style=hidden\n    set timeout=0\n",
                         "    set timeout_style=hidden\n    set timeout=\n", 1)
    assert empty != fast
    v = _verdict(empty)
    assert v.startswith("FAIL: ") and "menu timeout <empty>" in v
    empty_rf = fast.replace('= 1 ] ; then\n  set timeout=0\n', '= 1 ] ; then\n  set timeout=\n', 1)
    assert empty_rf != fast
    v = _verdict(empty_rf)
    assert v.startswith("FAIL: ") and "recordfail timeout is <empty>" in v


def test_verdict_fails_a_cfg_without_any_timeout_style():
    fast = _fixture("cam1-grub.cfg")
    v = _verdict(fast.replace("    set timeout_style=hidden\n", ""))
    assert v.startswith("FAIL: ") and "no timeout_style=hidden" in v


def test_verdict_grades_every_recordfail_branch_against_the_recordfail_timeout():
    # GRUB_BUTTON_CMOS_ADDRESS makes 00_header emit TWO make_timeout blocks; the second recordfail
    # branch is still a recordfail branch.
    fast = _fixture("cam1-grub.cfg")
    marker = "### END /etc/grub.d/00_header ###"
    second = ('if [ "${recordfail}" = 1 ] ; then\n  set timeout=30\nelse\n'
              '  set timeout=0\nfi\n')
    v = _verdict(fast.replace(marker, second + marker))
    assert v.startswith("FAIL: ") and "recordfail timeout is 30 s" in v
    assert "menu timeout" not in v


def test_lib_key_loops_do_not_depend_on_the_callers_ifs(tmp_path):
    f = tmp_path / "grub"
    f.write_text("GRUB_DEFAULT=saved\n")
    r = _lib(f'IFS=$\'\\n\'\ngrub_fast_boot_apply "{f}"\ngrub_fast_boot_is_applied "{f}"; '
             'echo "applied=$?"\ngrub_fast_boot_default_lines')
    assert r.returncode == 0, r.stderr
    assert f.read_text() == "GRUB_DEFAULT=saved\n" + WANT_LINES
    assert "applied=0" in r.stdout
    assert r.stdout.endswith(WANT_LINES)


def test_apply_refuses_an_unreadable_file_and_leaves_it_alone(tmp_path):
    f = tmp_path / "grub"
    f.write_text("GRUB_DEFAULT=saved\nGRUB_TIMEOUT=3\n")
    os.chmod(f, 0o200)
    try:
        assert not os.access(f, os.R_OK), (
            "run as non-root: root can read a 0200 file, so the unreadable case cannot be built")
        r = _apply(f)
        assert r.returncode != 0
        assert str(f) in r.stderr
    finally:
        os.chmod(f, 0o644)
    assert f.read_text() == "GRUB_DEFAULT=saved\nGRUB_TIMEOUT=3\n"


def test_apply_does_not_depend_on_the_callers_tmpdir(tmp_path):
    # The create-usb chroot inherits the host environment; a host TMPDIR that does not exist in
    # the chroot must not abort the image build. The temp is staged next to the target instead.
    f = tmp_path / "grub"
    f.write_text("GRUB_DEFAULT=saved\n")
    r = _lib(f'grub_fast_boot_apply "{f}"', env={"TMPDIR": str(tmp_path / "absent")})
    assert r.returncode == 0, r.stderr
    assert f.read_text() == "GRUB_DEFAULT=saved\n" + WANT_LINES
    assert sorted(p.name for p in tmp_path.iterdir()) == ["grub"], "no temp is left behind"


# =================================================================================================
# create-usb-linux.sh: the base image carries the settings, from the lib, and proves them
# =================================================================================================

SETUP_HEAD = "cat > \"$MOUNT_ROOT/tmp/setup.sh\" << 'SETUP_EOF'\n"


def _chroot_setup(text):
    start = text.find(SETUP_HEAD)
    assert start >= 0, "create-usb-linux.sh writes the chroot setup.sh"
    end = text.find("\nSETUP_EOF\n", start)
    assert end > start
    return text[start + len(SETUP_HEAD):end]


def _grub_heredoc(setup):
    head = "cat > /etc/default/grub << 'GRUBEOF'\n"
    s = setup.find(head)
    assert s >= 0, "the chroot still writes /etc/default/grub"
    e = setup.find("\nGRUBEOF\n", s)
    return s, setup[s + len(head):e]


def test_create_usb_grub_heredoc_does_not_retype_the_settings():
    _, body = _grub_heredoc(_chroot_setup(_read(CREATE_USB)))
    for key in ("GRUB_TIMEOUT", "GRUB_TIMEOUT_STYLE", "GRUB_RECORDFAIL_TIMEOUT"):
        assert not re.search(rf"(?m)^\s*{key}=", body), f"{key} comes from the lib, not the heredoc"
    # The rest of the base-image grub config is unchanged.
    for keep in ('GRUB_DEFAULT=saved', 'GRUB_CMDLINE_LINUX="console=tty0"',
                 'GRUB_TERMINAL="console"', 'GRUB_DISABLE_OS_PROBER=true'):
        assert keep in body.splitlines()


def test_create_usb_emitted_default_grub_carries_the_three_settings(tmp_path):
    setup = _chroot_setup(_read(CREATE_USB))
    s, _ = _grub_heredoc(setup)
    apply_at = setup.find("grub_fast_boot_apply /etc/default/grub", s)
    assert apply_at > s, "the chroot applies the lib to the grub file it just wrote"
    block = setup[s:setup.find("\n", apply_at)]
    assert re.search(r"(?m)^(source|\.) /tmp/grub-fast-boot\.sh$", block), block
    grub = tmp_path / "default-grub"
    script = (block.replace("/etc/default/grub", str(grub))
                   .replace("/tmp/grub-fast-boot.sh", LIB))
    r = _bash("set -euo pipefail\n" + script)
    assert r.returncode == 0, r.stdout + r.stderr
    out = grub.read_text()
    for line in WANT_LINES.splitlines() + ['GRUB_DEFAULT=saved', 'GRUB_CMDLINE_LINUX="console=tty0"',
                                            'GRUB_TERMINAL="console"', 'GRUB_DISABLE_OS_PROBER=true']:
        assert line in out.splitlines(), (line, out)
    r = _lib(f'grub_fast_boot_is_applied "{grub}"')
    assert r.returncode == 0


def test_create_usb_applies_before_update_grub():
    code = _code(_chroot_setup(_read(CREATE_USB)))
    apply_at = code.find("grub_fast_boot_apply /etc/default/grub")
    assert 0 <= apply_at < code.find("update-grub")


def test_create_usb_copies_the_lib_into_the_chroot_and_removes_it():
    text = _read(CREATE_USB)
    code = _code(text[text.find("\nSETUP_EOF\n"):])
    cp_at = code.find('cp "$SCRIPT_DIR/lib/grub-fast-boot.sh" "$MOUNT_ROOT/tmp/grub-fast-boot.sh"')
    run_at = code.find('chroot "$MOUNT_ROOT" /tmp/setup.sh')
    assert 0 <= cp_at < run_at
    rm = [ln for ln in code.splitlines() if ln.strip().startswith("rm -f") and "/tmp/setup.sh" in ln]
    assert rm and '"$MOUNT_ROOT/tmp/grub-fast-boot.sh"' in rm[0]


def test_create_usb_chroot_grades_the_generated_grub_cfg(tmp_path):
    setup = _chroot_setup(_read(CREATE_USB))
    start = setup.find("# Check GRUB fast boot")
    assert start >= 0, "the chroot verification grades the generated grub.cfg"
    end = setup.find("\nfi\n", start)
    block = setup[start:end + 4]
    assert "grub_fast_boot_cfg_verdict" in block and "ERRORS=$((ERRORS+1))" in block
    for name, want in (("cam1-grub.cfg", "0"), ("base-image-grub.cfg", "1")):
        script = (f'. "{LIB}"\nERRORS=0\n'
                  + block.replace("/boot/grub/grub.cfg", os.path.join(FIXTURES, name))
                  + '\necho "ERRORS=$ERRORS"\n')
        r = _bash(script)
        assert r.returncode == 0, r.stderr
        assert r.stdout.strip().splitlines()[-1] == f"ERRORS={want}", (name, r.stdout)


def test_create_usb_requires_the_lib_before_any_disk_write(tmp_path):
    scripts = tmp_path / "scripts"
    (scripts / "lib").mkdir(parents=True)
    (tmp_path / "systemd").mkdir()
    (scripts / "lib" / "install-grub-efi.sh").write_text("# stub\n")
    (scripts / "lib" / "camera-box-grow-root.sh").write_text("# stub\n")
    (tmp_path / "systemd" / "camera-box-grow-root.service").write_text("# stub\n")
    snippet = f"SCRIPT_DIR='{scripts}'\ncheck_required_files\necho SHOULD_NOT_REACH"
    r = _bash(f"CREATE_USB_SOURCE_ONLY=1 source '{CREATE_USB}'\n{snippet}")
    assert r.returncode != 0
    assert "grub-fast-boot.sh" in r.stdout + r.stderr
    assert "SHOULD_NOT_REACH" not in r.stdout
    real = os.path.join(REPO, "scripts")
    r = _bash(f"CREATE_USB_SOURCE_ONLY=1 source '{CREATE_USB}'\nSCRIPT_DIR='{real}'\ncheck_required_files")
    assert r.returncode == 0, r.stdout + r.stderr


# =================================================================================================
# build-image.sh: the ro-root+overlay image is a cambox image too
# =================================================================================================

def _install_bootloader(text):
    s = text.find("install_bootloader() {")
    assert s >= 0
    return text[s:text.find("\n}\n", s)]


def test_build_image_sources_the_lib():
    assert re.search(r'(?m)^\. "\$\{?SCRIPT_DIR\}?/lib/grub-fast-boot\.sh"', _read(BUILD_IMAGE))


def test_build_image_emitted_default_grub_carries_the_three_settings(tmp_path):
    func = _install_bootloader(_read(BUILD_IMAGE))
    head = 'cat > "${WORK_DIR}/rootfs/etc/default/grub" << \'EOF\'\n'
    s = func.find(head)
    assert s >= 0
    body = func[s + len(head):func.find("\nEOF\n", s)]
    for key in ("GRUB_TIMEOUT", "GRUB_TIMEOUT_STYLE", "GRUB_RECORDFAIL_TIMEOUT"):
        assert not re.search(rf"(?m)^\s*{key}=", body), f"{key} comes from the lib"
    apply_line = 'grub_fast_boot_apply "${WORK_DIR}/rootfs/etc/default/grub"'
    apply_at = func.find(apply_line, s)
    assert apply_at > s
    code = _code(func)
    assert code.find(apply_line) < code.find("update-grub")
    (tmp_path / "rootfs" / "etc" / "default").mkdir(parents=True)
    script = f'. "{LIB}"\nWORK_DIR="{tmp_path}"\n' + func[s:apply_at + len(apply_line)]
    r = _bash("set -euo pipefail\n" + script)
    assert r.returncode == 0, r.stdout + r.stderr
    out = (tmp_path / "rootfs" / "etc" / "default" / "grub").read_text()
    for line in WANT_LINES.splitlines() + ["GRUB_DEFAULT=saved", "GRUB_DISABLE_OS_PROBER=true"]:
        assert line in out.splitlines()


def test_build_image_grades_the_generated_grub_cfg_after_update_grub():
    func = _install_bootloader(_read(BUILD_IMAGE))
    code = _code(func)
    assert code.find("update-grub") < code.find("grub_fast_boot_cfg_verdict")
    # Run the real grading lines (from `local fast_verdict` through its `|| error` line) inside a
    # function, with error() stubbed: a slow cfg must abort the build, the fast one must pass.
    s = func.find("local fast_verdict")
    assert s >= 0
    e = func.find("\n", func.find('|| error "#1394', s))
    block = func[s:e]
    for name, want_rc in (("cam1-grub.cfg", 0), ("base-image-grub.cfg", 1)):
        script = (f'. "{LIB}"\nerror() {{ echo "ERR $*"; exit 1; }}\n'
                  f'grub_cfg="{os.path.join(FIXTURES, name)}"\n'
                  f"grade() {{\n{block}\n}}\ngrade\necho GRADED\n")
        r = _bash("set -euo pipefail\n" + script)
        assert r.returncode == want_rc, (name, r.stdout, r.stderr)
        if want_rc:
            assert "ERR #1394" in r.stdout and "recordfail timeout is 30 s" in r.stdout
            assert "GRADED" not in r.stdout
        else:
            assert r.stdout.strip() == "GRADED"


# =================================================================================================
# setup-device.sh STEP 10: the lib replaces the three sed/grep lines
# =================================================================================================

def _step10(text):
    s = text.find("# STEP 10: GRUB")
    e = text.find("# STEP 11:", s)
    assert 0 <= s < e
    return text[s:e]


def test_setup_device_sources_the_lib_before_step10():
    text = _read(SETUP_DEVICE)
    m = re.search(r'(?m)^\. "\$HERE/lib/grub-fast-boot\.sh"', text)
    assert m and m.start() < text.find("# STEP 10: GRUB")


def test_setup_device_step10_calls_the_lib_before_update_grub():
    code = _code(_step10(_read(SETUP_DEVICE)))
    apply_at = code.find("grub_fast_boot_apply /etc/default/grub")
    assert 0 <= apply_at < code.find("update-grub")
    assert "GRUB_TIMEOUT" not in code and "GRUB_RECORDFAIL_TIMEOUT" not in code, \
        "STEP 10 no longer carries its own copy of the settings"
    # The rest of STEP 10 is unchanged: the #295 saved default and the core-isolation flags.
    assert "GRUB_DEFAULT=saved" in code and "for flag_tag in " in code


def test_setup_device_step10_apply_line_runs_against_a_stock_file(tmp_path):
    code = _code(_step10(_read(SETUP_DEVICE)))
    line = next(ln for ln in code.splitlines() if "grub_fast_boot_apply" in ln)
    f = tmp_path / "grub"
    f.write_text("GRUB_DEFAULT=0\nGRUB_TIMEOUT_STYLE=hidden\nGRUB_TIMEOUT=10\n")
    r = _bash(f'set -euo pipefail\n. "{LIB}"\n' + line.replace("/etc/default/grub", str(f)))
    assert r.returncode == 0, r.stderr
    assert f.read_text() == ("GRUB_DEFAULT=0\nGRUB_TIMEOUT_STYLE=hidden\nGRUB_TIMEOUT=0\n"
                             "GRUB_RECORDFAIL_TIMEOUT=0\n")


# =================================================================================================
# verify-device.sh (aq): a hard gate over the box's generated grub.cfg
# =================================================================================================

def _aq_block(text):
    aq = text.find("\n# (aq) ")
    ao = text.find("\n# (ao) ", aq)
    assert 0 <= aq < ao, "(aq) sits right before (ao)"
    return text[aq:ao]


def test_verify_device_documents_sources_and_places_aq():
    text = _read(VERIFY_DEVICE)
    header = text[:text.find("set -euo pipefail")]
    assert "(aq)" in header and "GRUB" in header[header.find("(aq)"):]
    usage = text[text.find("usage() {"):]
    assert "(aq)" in usage[:usage.find("\nEOF\n")]
    assert re.search(r'(?m)^\. "\$HERE/lib/grub-fast-boot\.sh"', text)
    guard = text.find("never run the live SSH flow below.")
    aq = text.find("\n# (aq) ")
    q = text.rfind("# (q) .bak cruft drift")
    assert guard < text.find("\n# (am) ") < aq < text.find("\n# (ao) ") < q
    # Never inside the slices other pytests EXECUTE: (ao)..(an) and (an)..(q).
    assert not (text.find("\n# (ao) ") < aq < q)


def test_verify_device_aq_is_a_hard_gate():
    block = _aq_block(_read(VERIFY_DEVICE))
    code = _code(block)
    assert "grub_fast_boot_cfg_verdict" in code
    assert "cat /boot/grub/grub.cfg" in code
    assert 'fail "' in code and 'warn "' not in code


def _run_aq(ssh_body, ssh_rc=0):
    block = _aq_block(_read(VERIFY_DEVICE))
    prelude = (f'. "{LIB}"\n'
               'ok() { echo "OK $1"; }\nfail() { echo "FAIL $1"; }\n'
               f'ssh_box() {{ printf "%s" "$SSH_OUT"; return {ssh_rc}; }}\n')
    r = _bash("set -euo pipefail\n" + prelude + block, env={"SSH_OUT": ssh_body})
    assert r.returncode == 0, r.stdout + r.stderr
    return r.stdout


def test_verify_device_aq_passes_the_fast_cfg():
    out = _run_aq(_fixture("cam1-grub.cfg"))
    assert out.startswith("OK ") and "FAIL" not in out


def test_verify_device_aq_fails_the_30s_recordfail_cfgs():
    for name in ("base-image-grub.cfg", "hidden-recordfail30-grub.cfg"):
        out = _run_aq(_fixture(name))
        assert out.startswith("FAIL ") and "recordfail timeout is 30 s" in out, (name, out)


def test_verify_device_aq_fails_an_unreadable_cfg():
    out = _run_aq("", ssh_rc=255)
    assert out.startswith("FAIL ") and "could not read /boot/grub/grub.cfg" in out
    out = _run_aq("")
    assert out.startswith("FAIL ")
